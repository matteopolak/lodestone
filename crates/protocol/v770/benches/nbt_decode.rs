//! NBT decode-throughput benchmark (issue #78 epic, sub-issue #142).
//!
//! Exercises `lodestone_core::read_network_nbt`, the decoder every
//! NBT-carrying field in every protocol crate goes through (item components,
//! entity metadata, block entities, the registry-data configuration packets).
//! It lives in `lodestone-core`, not this crate, but this crate is the
//! concrete consumer for a hot NBT path — a chunk's block entities — so
//! benchmarking it here (with data shaped like what this crate actually
//! decodes) is in scope without touching `lodestone-core` itself.
//!
//! # Evidence caveat
//!
//! Same as `chunk_light_decode.rs`: the payload below is built with our own
//! `write_network_nbt`, not captured from a live server. For a *throughput*
//! benchmark (as opposed to a correctness test — NBT already has hermetic
//! round-trip and depth-limit coverage elsewhere) this is standard practice;
//! `serde_json`/`criterion` benchmark suites do the same. Flagged per
//! `CLAUDE.md`'s evidence standard rather than left implicit.
//!
//! Run with: `cargo bench -p lodestone-v770 --bench nbt_decode`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_core::{Nbt, NbtTag, Reader, Writer, read_network_nbt, write_network_nbt};

/// A compound shaped like a real "interesting" payload this codebase actually
/// decodes: an enchanted item's component patch has this rough shape (a small
/// compound with a nested list of nested compounds, some scalar leaves, one
/// string-keyed sub-compound) — see `tests/item_components.rs` /
/// `tests/fixtures/tool_component_explicit.hex` for the real analogue this
/// mirrors the *shape* of, without decoding those exact captured bytes here
/// (this file benchmarks the shared NBT layer, not the v770-specific
/// component-patch framing on top of it).
fn sample_nbt() -> Nbt {
    let enchantment = |name: &str, lvl: i32| {
        Nbt::Compound(vec![
            ("id".to_owned(), Nbt::String(format!("minecraft:{name}"))),
            ("lvl".to_owned(), Nbt::Int(lvl)),
        ])
    };
    Nbt::Compound(vec![
        ("display_name".to_owned(), Nbt::String("Excalibur".to_owned())),
        ("damage".to_owned(), Nbt::Int(1200)),
        ("max_damage".to_owned(), Nbt::Int(1561)),
        ("repair_cost".to_owned(), Nbt::Int(7)),
        (
            "enchantments".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![
                    enchantment("sharpness", 5),
                    enchantment("unbreaking", 3),
                    enchantment("mending", 1),
                    enchantment("looting", 3),
                ],
            },
        ),
        (
            "attribute_modifiers".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("type".to_owned(), Nbt::String("minecraft:attack_damage".to_owned())),
                    ("amount".to_owned(), Nbt::Double(7.0)),
                    ("operation".to_owned(), Nbt::String("add_value".to_owned())),
                ])],
            },
        ),
        ("lore".to_owned(), Nbt::List {
            element_type: NbtTag::String,
            elements: vec![
                Nbt::String("A legendary blade".to_owned()),
                Nbt::String("forged in starlight".to_owned()),
            ],
        }),
        ("custom_model_data".to_owned(), Nbt::IntArray(vec![1001, 1002, 1003])),
        ("unbreakable".to_owned(), Nbt::Byte(1)),
    ])
}

/// A block-entity-shaped payload: a chest's 27-slot inventory, each slot
/// either empty or a small compound — the real per-chunk NBT volume this
/// crate's chunk decode carries (`BlockEntity::decode_list`), scaled to a
/// single populated chest rather than a whole chunk's worth (chunk-scale
/// throughput is `chunk_light_decode.rs`'s job; this isolates the NBT layer).
fn chest_nbt() -> Nbt {
    let slot = |i: i32| {
        Nbt::Compound(vec![
            ("Slot".to_owned(), Nbt::Byte(i as i8)),
            ("id".to_owned(), Nbt::String("minecraft:diamond".to_owned())),
            ("count".to_owned(), Nbt::Int(1 + (i % 64))),
        ])
    };
    Nbt::Compound(vec![
        ("id".to_owned(), Nbt::String("minecraft:chest".to_owned())),
        ("x".to_owned(), Nbt::Int(120)),
        ("y".to_owned(), Nbt::Int(70)),
        ("z".to_owned(), Nbt::Int(-40)),
        (
            "Items".to_owned(),
            Nbt::List {
                element_type: NbtTag::Compound,
                elements: (0..27).map(slot).collect(),
            },
        ),
    ])
}

fn bench(c: &mut Criterion, name: &'static str, value: &Nbt) {
    let mut w = Writer::default();
    write_network_nbt(&mut w, value).expect("bench payload encodes");
    let bytes = w.into_vec();

    {
        let mut r = Reader::new(&bytes);
        let decoded = read_network_nbt(&mut r).expect("bench payload decodes");
        r.ensure_empty().expect("zero trailing bytes");
        black_box(&decoded);
    }

    let scene = format!("{name} ({} bytes)", bytes.len());
    const ITERS: usize = 5000;
    for _ in 0..50 {
        let mut r = Reader::new(&bytes);
        black_box(read_network_nbt(&mut r).unwrap());
    }
    let t0 = Instant::now();
    for _ in 0..ITERS {
        let mut r = Reader::new(black_box(&bytes));
        black_box(read_network_nbt(&mut r).unwrap());
    }
    let mean_ns = t0.elapsed().as_secs_f64() * 1e9 / ITERS as f64;
    support::record(support::Record {
        bench: "nbt_decode",
        metric: "decode_mean_ns",
        scene: &scene,
        value: mean_ns,
        unit: "ns",
    });
    println!("NBT decode [{name}]: {mean_ns:.0} ns/call mean over {ITERS} iters ({} bytes)", bytes.len());

    c.bench_function(&format!("protocol/nbt_decode_{name}"), |b| {
        b.iter(|| {
            let mut r = Reader::new(black_box(&bytes));
            black_box(read_network_nbt(&mut r).unwrap())
        })
    });
}

fn bench_item_component_shaped(c: &mut Criterion) {
    let value = sample_nbt();
    bench(c, "item_component_shaped", &value);
}

fn bench_chest_block_entity_shaped(c: &mut Criterion) {
    let value = chest_nbt();
    bench(c, "chest_block_entity_shaped", &value);
}

criterion_group!(benches, bench_item_component_shaped, bench_chest_block_entity_shaped);
criterion_main!(benches);
