//! Generate-or-assert gate for `src/biome_ambient/table.rs`.
//!
//! Same shape and same reasoning as `biome_music_table.rs`: the committed table must
//! be exactly what this generator produces from
//! `crates/lodestone-server/assets/worldgen/biome/*.json`, which is the
//! vanilla-derived **external oracle** these values come from.
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_ambient_table
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `AmbientMoodSettings.LEGACY_CAVE_SETTINGS` — `AmbientMoodSettings.java:19`. Every
/// biome that sets a mood in real data reuses these three numbers with only the sound
/// changed, which the value check below asserts.
const LEGACY_TICK_DELAY: i64 = 6_000;
const LEGACY_SEARCH_EXTENT: i64 = 8;
const LEGACY_OFFSET: f64 = 2.0;

fn biome_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-server/assets/worldgen/biome")
        .canonicalize()
        .expect("the biome asset directory must exist — it is this table's oracle")
}

fn table_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/biome_ambient/table.rs")
}

struct Mood {
    sound: String,
    tick_delay: i64,
    block_search_extent: i64,
    offset: f64,
}

struct Addition {
    sound: String,
    tick_chance: f64,
}

struct Parsed {
    loop_sound: Option<String>,
    mood: Option<Mood>,
    additions: Vec<Addition>,
}

fn strip_ns(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, p)| p)
}

fn parse_mood(biome: &str, v: &serde_json::Value) -> Mood {
    let o = v
        .as_object()
        .unwrap_or_else(|| panic!("{biome}: mood must be an object, got {v}"));
    for key in o.keys() {
        assert!(
            matches!(
                key.as_str(),
                "sound" | "tick_delay" | "block_search_extent" | "offset"
            ),
            "{biome}: unexpected key `{key}` in mood — AmbientMoodSettings.java:9 \
             declares exactly four"
        );
    }
    Mood {
        sound: strip_ns(
            o.get("sound")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| panic!("{biome}: mood has no string `sound`")),
        )
        .to_string(),
        tick_delay: o
            .get("tick_delay")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| panic!("{biome}: mood has no `tick_delay`")),
        block_search_extent: o
            .get("block_search_extent")
            .and_then(|v| v.as_i64())
            .unwrap_or_else(|| panic!("{biome}: mood has no `block_search_extent`")),
        offset: o
            .get("offset")
            .and_then(|v| v.as_f64())
            .unwrap_or_else(|| panic!("{biome}: mood has no `offset`")),
    }
}

fn parse_addition(biome: &str, v: &serde_json::Value) -> Addition {
    let o = v
        .as_object()
        .unwrap_or_else(|| panic!("{biome}: addition must be an object, got {v}"));
    Addition {
        sound: strip_ns(
            o.get("sound")
                .and_then(|s| s.as_str())
                .unwrap_or_else(|| panic!("{biome}: addition has no string `sound`")),
        )
        .to_string(),
        tick_chance: o
            .get("tick_chance")
            .and_then(|v| v.as_f64())
            .unwrap_or_else(|| panic!("{biome}: addition has no `tick_chance`")),
    }
}

fn parse_all() -> BTreeMap<String, Parsed> {
    let dir = biome_dir();
    let mut out = BTreeMap::new();
    let mut seen = 0usize;

    for entry in std::fs::read_dir(&dir).expect("read biome dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        seen += 1;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("utf-8 name")
            .to_string();
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
        let Some(a) = doc
            .get("attributes")
            .and_then(|a| a.as_object())
            .and_then(|a| a.get("minecraft:audio/ambient_sounds"))
        else {
            continue;
        };
        let o = a
            .as_object()
            .unwrap_or_else(|| panic!("{name}: audio/ambient_sounds must be an object"));
        for key in o.keys() {
            assert!(
                matches!(key.as_str(), "loop" | "mood" | "additions"),
                "{name}: unexpected key `{key}` — AmbientSounds.java:11 declares three"
            );
        }

        // `additions` uses a compact list codec (`AmbientSounds.java:20`), so it is
        // either a single object or an array. Both shapes must be accepted; real 26.2
        // data uses the single-object form, and assuming an array would panic.
        let additions = match o.get("additions") {
            None => Vec::new(),
            Some(serde_json::Value::Array(items)) => {
                items.iter().map(|i| parse_addition(&name, i)).collect()
            }
            Some(single) => vec![parse_addition(&name, single)],
        };

        out.insert(
            name.clone(),
            Parsed {
                loop_sound: o
                    .get("loop")
                    .and_then(|l| l.as_str())
                    .map(|l| strip_ns(l).to_string()),
                mood: o.get("mood").map(|m| parse_mood(&name, m)),
                additions,
            },
        );
    }

    assert!(
        seen >= 60,
        "only {seen} biome json files found in {} — the oracle path is wrong",
        dir.display()
    );
    // Exactly the five Nether biomes carry this attribute in 26.2. An equality rather
    // than a floor, so a change in either direction is reported.
    assert_eq!(
        out.len(),
        5,
        "expected exactly 5 biomes with audio/ambient_sounds (the Nether's), found {}: {:?}",
        out.len(),
        out.keys().collect::<Vec<_>>()
    );
    out
}

fn emit(parsed: &BTreeMap<String, Parsed>) -> String {
    let mut s = String::new();
    s.push_str(
        "//! GENERATED — do not edit by hand.\n\
         //!\n\
         //! Produced by `tests/biome_ambient_table.rs` from\n\
         //! `crates/lodestone-server/assets/worldgen/biome/*.json`. Refresh with:\n\
         //!\n\
         //! ```text\n\
         //! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_ambient_table\n\
         //! ```\n\
         //!\n\
         //! Sorted by biome id; `biome_ambient` binary-searches it.\n\n",
    );
    s.push_str("use std::borrow::Cow;\n\n");
    s.push_str(
        "use crate::ambient::{AmbientAdditionsSettings, AmbientMoodSettings, AmbientSounds};\n\n",
    );
    s.push_str("/// Every biome that declares `minecraft:audio/ambient_sounds`, sorted by id.\n");
    s.push_str("pub static BIOME_AMBIENT: &[(&str, AmbientSounds)] = &[\n");
    for (name, p) in parsed {
        s.push_str("    (\n");
        s.push_str(&format!("        \"{name}\",\n"));
        s.push_str("        AmbientSounds {\n");
        match &p.loop_sound {
            None => s.push_str("            loop_sound: None,\n"),
            Some(l) => s.push_str(&format!(
                "            loop_sound: Some(Cow::Borrowed(\"{l}\")),\n"
            )),
        }
        match &p.mood {
            None => s.push_str("            mood: None,\n"),
            Some(m) => s.push_str(&format!(
                "            mood: Some(AmbientMoodSettings::of(\"{}\", {}, {}, {:?})),\n",
                m.sound, m.tick_delay, m.block_search_extent, m.offset
            )),
        }
        if p.additions.is_empty() {
            s.push_str("            additions: Cow::Borrowed(&[]),\n");
        } else {
            s.push_str("            additions: Cow::Borrowed(&[\n");
            for a in &p.additions {
                s.push_str(&format!(
                    "                AmbientAdditionsSettings::of(\"{}\", {:?}),\n",
                    a.sound, a.tick_chance
                ));
            }
            s.push_str("            ]),\n");
        }
        s.push_str("        },\n");
        s.push_str("    ),\n");
    }
    s.push_str("];\n");
    s
}

#[test]
fn committed_table_matches_the_biome_assets() {
    let parsed = parse_all();
    let expected = emit(&parsed);
    let path = table_path();

    if std::env::var_os("LODESTONE_REGEN").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create biome_ambient dir");
        }
        std::fs::write(&path, &expected).expect("write generated table");
        eprintln!(
            "LODESTONE_REGEN: wrote {} ({} biomes)",
            path.display(),
            parsed.len()
        );
        return;
    }

    let actual = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {}: {e}. Generate it with LODESTONE_REGEN=1 cargo test \
             -p lodestone-sound --test biome_ambient_table",
            path.display()
        )
    });

    if actual != expected {
        let first = actual
            .lines()
            .zip(expected.lines())
            .position(|(a, b)| a != b);
        panic!(
            "src/biome_ambient/table.rs has drifted from the biome assets \
             (first differing line: {first:?}). Regenerate with LODESTONE_REGEN=1."
        );
    }
}

/// The values against the jar rather than against the table, so a wrong asset dump
/// cannot launder itself through a regeneration.
#[test]
fn every_biome_mood_reuses_the_legacy_cave_geometry() {
    let parsed = parse_all();

    // All five are the Nether's, and all five have all three components.
    let names: Vec<&str> = parsed.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        vec![
            "basalt_deltas",
            "crimson_forest",
            "nether_wastes",
            "soul_sand_valley",
            "warped_forest"
        ]
    );

    for (name, p) in &parsed {
        let mood = p
            .mood
            .as_ref()
            .unwrap_or_else(|| panic!("{name} should declare a mood"));
        // Only the *sound* differs from LEGACY_CAVE_SETTINGS; the geometry is shared.
        assert_eq!(
            (mood.tick_delay, mood.block_search_extent, mood.offset),
            (LEGACY_TICK_DELAY, LEGACY_SEARCH_EXTENT, LEGACY_OFFSET),
            "{name}: mood geometry diverges from AmbientMoodSettings.java:19"
        );
        assert_eq!(mood.sound, format!("ambient.{name}.mood"));

        let loop_sound = p
            .loop_sound
            .as_ref()
            .unwrap_or_else(|| panic!("{name} should declare a loop"));
        assert_eq!(*loop_sound, format!("ambient.{name}.loop"));

        assert_eq!(p.additions.len(), 1, "{name} should have one addition");
        let a = &p.additions[0];
        assert_eq!(a.sound, format!("ambient.{name}.additions"));
        // 0.0111 per tick ~ once every 90 ticks. Asserted exactly, because a
        // plausible-looking 0.111 or 0.00111 is a 10x cadence error that a
        // "fires sometimes" test cannot see.
        assert_eq!(
            a.tick_chance, 0.0111,
            "{name}: addition tick_chance must be exactly 0.0111"
        );
    }
}
