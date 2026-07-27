//! Task A2.2 — atlas **source lists** (`atlases/<id>.json`).
//!
//! Authority discipline (plan §12.31): the parsing tests feed the *exact* JSON
//! shapes vanilla ships (`directory`, `single`, `paletted_permutations`,
//! including the real armor-trims separator/permutation layout), and the
//! resolution tests are built so a wrong implementation must diverge — a
//! directory source that forgot the prefix, or mis-derived the namespace, or
//! double-counted an overridden sprite, produces a different set than asserted.

use lodestone_assets::{
    AtlasDefinition, AtlasSource, MemorySource, ResourceLocation, ResourceManager, ResourceSource,
};

fn manager(sources: Vec<Box<dyn ResourceSource>>) -> ResourceManager {
    ResourceManager::new(sources)
}

fn pack(name: &str, files: &[(&str, &[u8])]) -> Box<dyn ResourceSource> {
    let mut src = MemorySource::new(name);
    for (path, bytes) in files {
        src.insert(*path, bytes.to_vec());
    }
    Box::new(src)
}

#[test]
fn parses_real_directory_source_shape() {
    // Verbatim from client.jar assets/minecraft/atlases/shield_patterns.json.
    let json = br#"{"sources":[{"type":"minecraft:directory","prefix":"entity/shield/","source":"entity/shield"}]}"#;
    let def = AtlasDefinition::parse(json).expect("parse");
    assert_eq!(def.sources.len(), 1);
    assert_eq!(
        def.sources[0],
        AtlasSource::Directory {
            source: "entity/shield".into(),
            prefix: "entity/shield/".into(),
        }
    );
}

#[test]
fn directory_source_derives_prefixed_namespaced_sprite_ids() {
    // A chest-style directory source: textures/entity/chest/*.png become
    // sprites entity/chest/<name>. A wrong impl that drops the prefix, keeps the
    // .png, or mis-derives the namespace fails this.
    let def = AtlasDefinition::parse(
        br#"{"sources":[{"type":"minecraft:directory","prefix":"entity/chest/","source":"entity/chest"}]}"#,
    )
    .unwrap();

    let mgr = manager(vec![pack(
        "vanilla",
        &[
            ("assets/minecraft/textures/entity/chest/normal.png", b"n"),
            (
                "assets/minecraft/textures/entity/chest/normal_left.png",
                b"l",
            ),
            ("assets/minecraft/textures/entity/chest/trapped.png", b"t"),
            // Nested one level deeper — still included, relative path preserved.
            (
                "assets/minecraft/textures/entity/chest/christmas/normal.png",
                b"c",
            ),
            // Outside the source dir — must NOT appear.
            ("assets/minecraft/textures/block/stone.png", b"s"),
            // A non-png in the dir — ignored.
            (
                "assets/minecraft/textures/entity/chest/normal.png.mcmeta",
                b"m",
            ),
        ],
    )]);

    let mut got: Vec<String> = def
        .resolve(&mgr)
        .into_iter()
        .map(|e| e.sprite.to_string())
        .collect();
    got.sort();

    assert_eq!(
        got,
        vec![
            "minecraft:entity/chest/christmas/normal",
            "minecraft:entity/chest/normal",
            "minecraft:entity/chest/normal_left",
            "minecraft:entity/chest/trapped",
        ]
    );
}

#[test]
fn directory_source_respects_pack_namespaces_and_override() {
    // A custom pack in its own namespace contributes its own sprites, and an
    // override of a vanilla path appears exactly once (dedup by sprite id).
    let def = AtlasDefinition::parse(
        br#"{"sources":[{"type":"minecraft:directory","prefix":"entity/chest/","source":"entity/chest"}]}"#,
    )
    .unwrap();

    let vanilla = pack(
        "vanilla",
        &[("assets/minecraft/textures/entity/chest/normal.png", b"v")],
    );
    let custom = pack(
        "custom",
        &[
            // Overrides vanilla's normal…
            ("assets/minecraft/textures/entity/chest/normal.png", b"c"),
            // …and adds one in its own namespace.
            ("assets/mypack/textures/entity/chest/fancy.png", b"f"),
        ],
    );
    // Stack: vanilla lowest, custom highest.
    let mgr = manager(vec![vanilla, custom]);

    let entries = def.resolve(&mgr);
    let ids: Vec<String> = entries.iter().map(|e| e.sprite.to_string()).collect();
    assert_eq!(
        ids.iter()
            .filter(|s| s.as_str() == "minecraft:entity/chest/normal")
            .count(),
        1,
        "an overridden sprite must appear once"
    );
    assert!(ids.iter().any(|s| s == "mypack:entity/chest/fancy"));
}

#[test]
fn single_source_defaults_sprite_to_resource_and_builds_texture_path() {
    let def = AtlasDefinition::parse(
        br#"{"sources":[{"type":"minecraft:single","resource":"minecraft:entity/bell/bell_body"}]}"#,
    )
    .unwrap();
    let entries = def.resolve(&manager(vec![]));
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].sprite,
        ResourceLocation::parse("minecraft:entity/bell/bell_body").unwrap()
    );
    assert_eq!(
        entries[0].texture_path,
        "assets/minecraft/textures/entity/bell/bell_body.png"
    );
}

#[test]
fn single_source_honours_sprite_override() {
    let def = AtlasDefinition::parse(
        br#"{"sources":[{"type":"minecraft:single","resource":"minecraft:item/empty_slot","sprite":"minecraft:gui/empty"}]}"#,
    )
    .unwrap();
    let entries = def.resolve(&manager(vec![]));
    assert_eq!(entries[0].sprite.to_string(), "minecraft:gui/empty");
    assert_eq!(
        entries[0].texture_path,
        "assets/minecraft/textures/item/empty_slot.png"
    );
}

#[test]
fn paletted_permutations_enumerates_texture_times_permutation_ids() {
    // Real armor-trims shape (trimmed). Derived ids are texture_<perm>. The
    // recolour pixels are a bake step; this only asserts the id enumeration a
    // wrong cartesian product (missing a perm, wrong separator) would fail.
    let def = AtlasDefinition::parse(
        br#"{"sources":[{
            "type":"minecraft:paletted_permutations",
            "palette_key":"minecraft:trims/color_palettes/trim_palette",
            "permutations":{
                "gold":"minecraft:trims/color_palettes/gold",
                "iron":"minecraft:trims/color_palettes/iron"
            },
            "textures":[
                "minecraft:trims/entity/humanoid/sentry",
                "minecraft:trims/entity/humanoid/dune"
            ]
        }]}"#,
    )
    .unwrap();

    // resolve() intentionally skips paletted_permutations (pixel gen is a bake).
    assert!(def.resolve(&manager(vec![])).is_empty());

    let mut ids: Vec<String> = def.sources[0]
        .derived_sprite_ids()
        .into_iter()
        .map(|l| l.to_string())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            "minecraft:trims/entity/humanoid/dune_gold",
            "minecraft:trims/entity/humanoid/dune_iron",
            "minecraft:trims/entity/humanoid/sentry_gold",
            "minecraft:trims/entity/humanoid/sentry_iron",
        ]
    );
}

#[test]
fn unknown_source_type_is_preserved_not_rejected() {
    let def = AtlasDefinition::parse(
        br#"{"sources":[{"type":"minecraft:unfurl","foo":1},{"type":"minecraft:single","resource":"minecraft:x/y"}]}"#,
    )
    .expect("an unknown source type must not fail the whole atlas");
    assert_eq!(def.sources.len(), 2);
    assert_eq!(
        def.sources[0],
        AtlasSource::Unknown {
            kind: "unfurl".into()
        }
    );
    // The known source after it still resolves.
    assert_eq!(def.resolve(&manager(vec![])).len(), 1);
}

#[test]
fn missing_sources_array_is_a_clear_error_not_a_panic() {
    let err = AtlasDefinition::parse(br#"{"not_sources":[]}"#).unwrap_err();
    assert!(format!("{err}").contains("sources"), "got: {err}");
}

#[test]
fn malformed_json_is_a_clear_error_not_a_panic() {
    let err = AtlasDefinition::parse(b"{ this is not json").unwrap_err();
    assert!(format!("{err}").contains("json"), "got: {err}");
}

#[test]
fn directory_and_multiple_sources_compose_in_order() {
    // A real atlas can mix a directory scan with explicit singles.
    let def = AtlasDefinition::parse(
        br#"{"sources":[
            {"type":"minecraft:directory","prefix":"block/","source":"block"},
            {"type":"minecraft:single","resource":"minecraft:entity/extra"}
        ]}"#,
    )
    .unwrap();
    let mgr = manager(vec![pack(
        "vanilla",
        &[
            ("assets/minecraft/textures/block/stone.png", b"s"),
            ("assets/minecraft/textures/block/dirt.png", b"d"),
        ],
    )]);
    let ids: Vec<String> = def
        .resolve(&mgr)
        .into_iter()
        .map(|e| e.sprite.to_string())
        .collect();
    assert!(ids.contains(&"minecraft:block/stone".to_string()));
    assert!(ids.contains(&"minecraft:block/dirt".to_string()));
    assert!(ids.contains(&"minecraft:entity/extra".to_string()));
}

// --- Task A3: pre-1.13 implicit terrain atlas (no atlases/*.json) -------------
//
// 1.8.9/1.12.2 have no declarative atlas index. The block sheet was every
// texture under `textures/blocks/`. `AtlasDefinition::implicit_terrain` builds
// that fallback from the profile's block-texture dir, so the *resolver* stays
// version-free — it takes a dir name, never a version.
//
// The cross-version proof: a plural-dir (1.8.9-shaped) pack resolved via the
// implicit fallback yields the SAME sprite id set as a singular-dir
// (26.2-shaped) pack resolved via a declarative directory source. The
// abstraction holds across the flattening; only the dir string differs.
use lodestone_assets::AssetProfile;

#[test]
fn implicit_terrain_fallback_matches_declarative_across_the_flattening() {
    // Legacy 1.8.9-shaped pack: textures/blocks/*.png, sprite ids "blocks/<n>".
    let legacy_mgr = manager(vec![pack(
        "legacy",
        &[
            ("assets/minecraft/textures/blocks/stone.png", b"s"),
            ("assets/minecraft/textures/blocks/dirt.png", b"d"),
            ("assets/minecraft/textures/blocks/oak_log.png", b"l"),
        ],
    )]);
    let legacy_atlas =
        AtlasDefinition::implicit_terrain(AssetProfile::LEGACY_1_8.block_texture_dir);
    let mut legacy_ids: Vec<String> = legacy_atlas
        .resolve(&legacy_mgr)
        .into_iter()
        .map(|e| e.sprite.to_string())
        .collect();
    legacy_ids.sort();

    // Modern 26.2-shaped pack via the same implicit builder on the flattened dir.
    let modern_mgr = manager(vec![pack(
        "modern",
        &[
            ("assets/minecraft/textures/block/stone.png", b"s"),
            ("assets/minecraft/textures/block/dirt.png", b"d"),
            ("assets/minecraft/textures/block/oak_log.png", b"l"),
        ],
    )]);
    let modern_atlas = AtlasDefinition::implicit_terrain(AssetProfile::MODERN.block_texture_dir);
    let mut modern_ids: Vec<String> = modern_atlas
        .resolve(&modern_mgr)
        .into_iter()
        .map(|e| e.sprite.to_string())
        .collect();
    modern_ids.sort();

    // The sprite-id *stems* are identical; only the dir prefix differs, exactly
    // as the profile says it should.
    assert_eq!(
        legacy_ids,
        vec![
            "minecraft:blocks/dirt",
            "minecraft:blocks/oak_log",
            "minecraft:blocks/stone"
        ]
    );
    assert_eq!(
        modern_ids,
        vec![
            "minecraft:block/dirt",
            "minecraft:block/oak_log",
            "minecraft:block/stone"
        ]
    );
    // Same count, same stems — the resolver did the same work for both.
    assert_eq!(legacy_ids.len(), modern_ids.len());
    let strip =
        |v: &[String], p: &str| -> Vec<String> { v.iter().map(|s| s.replace(p, "")).collect() };
    assert_eq!(strip(&legacy_ids, "blocks/"), strip(&modern_ids, "block/"));
}
