//! Generate-or-assert gate for `src/biome_music/table.rs`.
//!
//! The committed table must be exactly what this generator produces from
//! `crates/lodestone-server/assets/worldgen/biome/*.json`. Those JSON files are
//! vanilla-derived, so they are the **external oracle** this table's values
//! originate from — the property `decode(encode(x)) == x` does not have and the
//! reason a hand-transcribed table (the existing precedent in `lodestone-assets`'
//! `tint.rs`) would not be good enough evidence.
//!
//! To refresh after the assets change:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table
//! ```
//!
//! Note what this gate does **not** prove: that any of the chosen tracks exist on
//! disk. They do not, on a default checkout — music is behind
//! `cargo xtask fetch-sounds --all`. That is
//! `tests/music_selection.rs`'s missing-asset gate, not this one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Vanilla's own game-music helper's delays, used only to sanity
/// check that the data says what the jar says.
const GAME_MIN: i64 = 12_000;
const GAME_MAX: i64 = 24_000;

fn biome_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-server/assets/worldgen/biome")
        .canonicalize()
        .expect(
            "the biome asset directory must exist — it is the oracle for this table. \
             Expected crates/lodestone-server/assets/worldgen/biome",
        )
}

fn table_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/biome_music/table.rs")
}

/// One parsed `Music` record out of the JSON.
struct Track {
    sound: String,
    min_delay: i64,
    max_delay: i64,
    replace: bool,
}

/// One biome's parsed audio attributes.
struct Parsed {
    default: Option<Track>,
    creative: Option<Track>,
    underwater: Option<Track>,
    music_volume: Option<f64>,
}

fn strip_ns(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, p)| p)
}

/// Parses one `Music` record. Anything other than the plain object shape is a hard
/// error: `lodestone_v26_2::packets::registry::biome_sky_color` has to cope with
/// an `Int`/`String`/modifier-compound union because it reads *wire* NBT, but these
/// are our own committed JSON files and a surprise shape there means the assets
/// changed in a way this generator has not been taught. Failing loudly beats
/// emitting a silently incomplete table.
fn parse_track(biome: &str, slot: &str, value: &serde_json::Value) -> Track {
    let obj = value.as_object().unwrap_or_else(|| {
        panic!("{biome}: attributes.audio/background_music.{slot} must be an object, got {value}")
    });
    let sound = obj
        .get("sound")
        .and_then(|s| s.as_str())
        .unwrap_or_else(|| panic!("{biome}: {slot} has no string `sound`"));
    let min_delay = obj
        .get("min_delay")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| panic!("{biome}: {slot} has no integer `min_delay`"));
    let max_delay = obj
        .get("max_delay")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| panic!("{biome}: {slot} has no integer `max_delay`"));
    let replace = obj
        .get("replace_current_music")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    for key in obj.keys() {
        assert!(
            matches!(
                key.as_str(),
                "sound" | "min_delay" | "max_delay" | "replace_current_music"
            ),
            "{biome}: unexpected key `{key}` in {slot} — vanilla's own music record grew a field \
             and this generator needs teaching"
        );
    }
    Track {
        sound: strip_ns(sound).to_string(),
        min_delay,
        max_delay,
        replace,
    }
}

/// Reads every biome document and returns the ones with audio attributes.
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
            .expect("utf-8 biome file name")
            .to_string();
        let bytes = std::fs::read(&path).expect("read biome json");
        let doc: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{name}: bad json: {e}"));

        let Some(attrs) = doc.get("attributes").and_then(|a| a.as_object()) else {
            continue;
        };
        let music = attrs.get("minecraft:audio/background_music");
        let volume = attrs
            .get("minecraft:audio/music_volume")
            .and_then(|v| v.as_f64());
        if music.is_none() && volume.is_none() {
            continue;
        }

        let (default, creative, underwater) = match music {
            Some(m) => {
                let obj = m.as_object().unwrap_or_else(|| {
                    panic!("{name}: audio/background_music must be an object, got {m}")
                });
                for key in obj.keys() {
                    assert!(
                        matches!(key.as_str(), "default" | "creative" | "underwater"),
                        "{name}: unexpected slot `{key}` in audio/background_music \
                         (vanilla's own background-music record declares exactly three)"
                    );
                }
                (
                    obj.get("default").map(|v| parse_track(&name, "default", v)),
                    obj.get("creative")
                        .map(|v| parse_track(&name, "creative", v)),
                    obj.get("underwater")
                        .map(|v| parse_track(&name, "underwater", v)),
                )
            }
            None => (None, None, None),
        };

        out.insert(
            name,
            Parsed {
                default,
                creative,
                underwater,
                music_volume: volume,
            },
        );
    }

    // A precondition, not a skip: an empty or tiny read means the oracle path is
    // wrong and every assertion below would pass vacuously.
    assert!(
        seen >= 60,
        "only {seen} biome json files found in {} — the oracle path is wrong",
        dir.display()
    );
    assert!(
        out.len() >= 40,
        "only {} biomes carry audio attributes; expected 42+ — is `attributes` \
         still the key?",
        out.len()
    );
    out
}

fn emit_track(slot: &str, track: &Option<Track>) -> String {
    match track {
        None => format!("                {slot}: None,\n"),
        Some(t) => format!(
            "                {slot}: Some(Music::of(\"{}\", {}, {}, {})),\n",
            t.sound, t.min_delay, t.max_delay, t.replace
        ),
    }
}

fn emit(parsed: &BTreeMap<String, Parsed>) -> String {
    let mut s = String::new();
    s.push_str(
        "//! GENERATED — do not edit by hand.\n\
         //!\n\
         //! Produced by `tests/biome_music_table.rs` from\n\
         //! `crates/lodestone-server/assets/worldgen/biome/*.json`, which is where the\n\
         //! authority for these values lives. Refresh with:\n\
         //!\n\
         //! ```text\n\
         //! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table\n\
         //! ```\n\
         //!\n\
         //! Sorted by biome id; `biome_audio` binary-searches it.\n\n",
    );
    s.push_str("use super::BiomeMusic;\n");
    s.push_str("use crate::music::{BackgroundMusic, Music};\n\n");
    s.push_str("/// Every biome that declares `minecraft:audio/background_music` or\n");
    s.push_str("/// `minecraft:audio/music_volume`, namespace-stripped and sorted by id.\n");
    s.push_str("pub static BIOME_MUSIC: &[(&str, BiomeMusic)] = &[\n");
    for (name, p) in parsed {
        s.push_str("    (\n");
        s.push_str(&format!("        \"{name}\",\n"));
        s.push_str("        BiomeMusic {\n");
        s.push_str("            music: BackgroundMusic {\n");
        s.push_str(&emit_track("default", &p.default));
        s.push_str(&emit_track("creative", &p.creative));
        s.push_str(&emit_track("underwater", &p.underwater));
        s.push_str("            },\n");
        match p.music_volume {
            None => s.push_str("            music_volume: None,\n"),
            Some(v) => s.push_str(&format!("            music_volume: Some({v:?}),\n")),
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
            std::fs::create_dir_all(parent).expect("create biome_music dir");
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
            "cannot read {}: {e}. Generate it with \
             LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table",
            path.display()
        )
    });

    if actual != expected {
        // Report *where*, not just that it differs — a whole-file diff of a 42-entry
        // table is unreadable and the useful signal is the first divergent line.
        let first = actual
            .lines()
            .zip(expected.lines())
            .position(|(a, b)| a != b);
        panic!(
            "src/biome_music/table.rs has drifted from the biome assets \
             (first differing line: {first:?}; committed {} lines, generated {} lines). \
             Regenerate with LODESTONE_REGEN=1 cargo test -p lodestone-sound \
             --test biome_music_table",
            actual.lines().count(),
            expected.lines().count()
        );
    }
}

/// The values themselves, checked against the jar rather than against the table.
///
/// This is separate from the drift check on purpose: the drift check proves the
/// table matches the JSON, and this proves the *JSON* matches what vanilla's own
/// music-selection helper says, so a wrong asset dump cannot launder itself through a regenerated table.
#[test]
fn every_biome_default_track_uses_the_jars_game_music_delays() {
    let parsed = parse_all();
    let mut checked = 0usize;
    for (name, p) in &parsed {
        for (slot, track) in [
            ("default", &p.default),
            ("creative", &p.creative),
            ("underwater", &p.underwater),
        ] {
            let Some(t) = track else { continue };
            checked += 1;
            assert_eq!(
                (t.min_delay, t.max_delay),
                (GAME_MIN, GAME_MAX),
                "{name}/{slot} ({}) has delays {}..={}, but every biome slot in vanilla is a \
                 game-music track (vanilla's own music-selection helper) at {GAME_MIN}..={GAME_MAX}",
                t.sound,
                t.min_delay,
                t.max_delay
            );
            assert!(
                !t.replace,
                "{name}/{slot} is flagged replace_current_music; vanilla's own game-music \
                 constructor passes false, and a replacing biome track would restart on every biome step"
            );
            assert!(
                t.sound.starts_with("music."),
                "{name}/{slot} sound `{}` is not a music event",
                t.sound
            );
        }
    }
    assert!(
        checked >= 50,
        "only {checked} track slots checked — expected 60+ across 42 biomes"
    );
}
