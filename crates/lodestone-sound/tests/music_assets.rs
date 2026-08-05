//! Do the identifiers music selection produces actually name real sound events?
//!
//! # Why this exists separately from `music_selection.rs`
//!
//! Those gates prove the *right identifier* is chosen for a situation, against the
//! jar. They cannot prove the identifier is a **real event name**: a table full of
//! plausible-looking typos (`music.overworld.jungle` vs `music.biome.jungle`)
//! satisfies every one of them, and the symptom in play would be silence that looks
//! exactly like the missing-asset case the design deliberately tolerates. That is
//! the worst possible failure mode — a real bug hiding inside an expected
//! degradation.
//!
//! So this gate joins the two: every one of the 42 generated biome tracks and all
//! seven `Musics` constants must resolve against the **real `sounds.json`** shipped
//! by 26.2, read out of the asset object store. That file is external to this
//! codebase and is present on a default checkout (it comes with `fetch-assets`, not
//! `fetch-sounds`), which is what makes it usable as an oracle here.
//!
//! # Ignored, because it needs the asset store
//!
//! Per this repo's convention an `#[ignore]`d test that has been *asked* to run
//! treats a missing precondition as a **failure, not a skip** — a test that quietly
//! passes when it could not find its input is the "precondition" species of vacuous
//! test. Run it with:
//!
//! ```text
//! cargo test -p lodestone-sound --test music_assets -- --ignored --nocapture
//! ```

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use lodestone_sound::biome_music::BIOME_MUSIC;
use lodestone_sound::music::musics;

/// Walks up from the crate to find `.cache/mc/<version>` holding both an
/// `asset-index-*.json` and the `objects/` tree, mirroring the shell's
/// `discover_store_root`.
fn store_root() -> PathBuf {
    let mut dir: &Path = Path::new(env!("CARGO_MANIFEST_DIR"));
    loop {
        let cache = dir.join(".cache/mc");
        if cache.is_dir() {
            let mut versions: Vec<PathBuf> = std::fs::read_dir(&cache)
                .expect("read .cache/mc")
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.join("objects").is_dir())
                .collect();
            versions.sort();
            if let Some(v) = versions.pop() {
                return v;
            }
        }
        dir = dir.parent().unwrap_or_else(|| {
            panic!(
                "no .cache/mc/<version> with an objects/ dir found above {}. \
                 This test needs the asset store: run `cargo run -p xtask -- fetch-assets`."
            , env!("CARGO_MANIFEST_DIR"))
        });
    }
}

/// Reads `minecraft/sounds.json` out of the content-addressed object store.
fn read_sounds_json(root: &Path) -> serde_json::Value {
    let index = std::fs::read_dir(root)
        .expect("read store root")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("asset-index-") && n.ends_with(".json"))
        })
        .unwrap_or_else(|| panic!("no asset-index-*.json in {}", root.display()));

    let index: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&index).expect("read asset index"))
            .expect("parse asset index");
    let hash = index
        .get("objects")
        .and_then(|o| o.get("minecraft/sounds.json"))
        .and_then(|o| o.get("hash"))
        .and_then(|h| h.as_str())
        .expect("asset index must declare minecraft/sounds.json");

    let path = root.join("objects").join(&hash[..2]).join(hash);
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "sounds.json object missing at {} ({e}). Run `cargo run -p xtask -- fetch-assets`.",
            path.display()
        )
    });
    serde_json::from_slice(&bytes).expect("parse sounds.json")
}

/// Every event name our music layer can ever ask for.
fn every_music_identifier() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for m in [
        &musics::MENU,
        &musics::CREATIVE,
        &musics::CREDITS,
        &musics::END_BOSS,
        &musics::END,
        &musics::UNDER_WATER,
        &musics::GAME,
    ] {
        out.insert(m.sound().to_string());
    }
    for (_, entry) in BIOME_MUSIC {
        for slot in [
            &entry.music.default,
            &entry.music.creative,
            &entry.music.underwater,
        ] {
            if let Some(track) = slot {
                out.insert(track.sound().to_string());
            }
        }
    }
    out
}

#[test]
#[ignore = "needs the asset object store (cargo run -p xtask -- fetch-assets)"]
fn every_selectable_track_is_a_real_event_in_the_shipped_sounds_json() {
    let root = store_root();
    let sounds = read_sounds_json(&root);
    let events = sounds
        .as_object()
        .expect("sounds.json is an object of event -> definition");

    // Precondition as a failure, not a skip.
    assert!(
        events.len() > 1_000,
        "sounds.json has only {} events — that is not the real 26.2 file",
        events.len()
    );

    let ours = every_music_identifier();
    assert!(
        ours.len() >= 25,
        "only {} distinct music identifiers collected; the table looks empty",
        ours.len()
    );

    let missing: Vec<&String> = ours.iter().filter(|k| !events.contains_key(*k)).collect();
    assert!(
        missing.is_empty(),
        "these music identifiers do not exist in the shipped sounds.json — they would \
         be silent in play and indistinguishable from the tolerated missing-asset case: \
         {missing:?}"
    );

    eprintln!(
        "verified {} music identifiers against {} events in {}",
        ours.len(),
        events.len(),
        root.display()
    );

    // The negative control for the join itself: a deliberately misspelled name in
    // the same shape must NOT be found, proving `contains_key` discriminates rather
    // than always answering yes.
    assert!(
        !events.contains_key("music.overworld.jungel"),
        "the lookup accepts a misspelling — this gate cannot detect a typo"
    );
}

/// Every music entry declares `"stream": true`, which is vanilla stating that these
/// must not be decoded eagerly. `SoundResolver::resolve_streaming` exists because of
/// this; `resolve_instance` would cache ~304 MiB for a single track.
#[test]
#[ignore = "needs the asset object store (cargo run -p xtask -- fetch-assets)"]
fn every_music_event_declares_stream_true() {
    let root = store_root();
    let sounds = read_sounds_json(&root);
    let events = sounds.as_object().expect("object");

    let mut checked = 0usize;
    let mut chained = 0usize;
    let mut empty = Vec::new();
    for name in every_music_identifier() {
        let def = events
            .get(&name)
            .unwrap_or_else(|| panic!("{name} not in sounds.json"));
        let list = def
            .get("sounds")
            .and_then(|s| s.as_array())
            .unwrap_or_else(|| panic!("{name} has no `sounds` array"));
        if list.is_empty() {
            empty.push(name.clone());
            continue;
        }

        for entry in list {
            // A bare string entry is a file reference with all defaults, so
            // `stream` would be false. Vanilla uses the object form for music.
            let obj = entry.as_object().unwrap_or_else(|| {
                panic!("{name}: expected object entries for a music event, got {entry}")
            });
            if obj.get("type").and_then(|t| t.as_str()) == Some("event") {
                // `music.creative` chains to `music.game` (verified on 26.2), whose
                // own leaves carry the flag. Chained entries have no stream flag of
                // their own.
                chained += 1;
                continue;
            }
            let stream = obj.get("stream").and_then(|s| s.as_bool()).unwrap_or(false);
            assert!(
                stream,
                "{name} entry {:?} is not `stream: true`; decoding it eagerly would \
                 allocate hundreds of MiB",
                obj.get("name")
            );
            checked += 1;
        }
    }

    assert!(
        checked >= 60,
        "only {checked} leaf entries checked — expected 60+ across the music corpus"
    );
    // `music.creative` referencing `music.game` is the one chain in the music set;
    // assert we saw it, so the `continue` above is a known case rather than a hole
    // that silently skipped everything.
    assert_eq!(
        chained, 1,
        "expected exactly one `type: event` chain (music.creative -> music.game), saw {chained}"
    );

    // A genuine 26.2 data quirk, found by this gate rather than assumed: exactly
    // one music event is declared with an **empty `sounds` array**, so it resolves
    // to vanilla's "empty sound" and the warped forest plays no music *even with
    // the full `--all` corpus on disk*.
    //
    // This matters more than a curiosity. It means the silence path is reached in
    // ordinary play, not only on an unfetched checkout — so "a missing track is
    // silence, not a panic" is load-bearing for a shipped configuration, and
    // treating an unresolvable music event as an error would break a vanilla biome.
    // Pinned as an equality so that if Mojang fills it in, this tells us rather
    // than silently passing.
    assert_eq!(
        empty,
        vec!["music.nether.warped_forest".to_string()],
        "the set of music events with empty `sounds` arrays changed"
    );
    eprintln!(
        "{checked} music leaf entries all declare stream: true; {chained} chained; \
         {} empty by data ({empty:?})",
        empty.len()
    );
}
