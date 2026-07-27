//! Tests for [`ResourceManager`], the ordered pack stack.

use lodestone_assets::{MemorySource, ResourceLocation, ResourceManager, ResourceSource};

fn src(name: &str, entries: &[(&str, &[u8])]) -> Box<dyn ResourceSource> {
    let mut s = MemorySource::new(name);
    for (path, bytes) in entries {
        s.insert(*path, bytes.to_vec());
    }
    Box::new(s)
}

#[test]
fn higher_priority_pack_wins() {
    // sources are lowest-priority first
    let manager = ResourceManager::new(vec![
        src("vanilla", &[("assets/minecraft/x.txt", b"vanilla")]),
        src("user", &[("assets/minecraft/x.txt", b"user")]),
    ]);
    assert_eq!(
        manager.read("assets/minecraft/x.txt"),
        Some(b"user".to_vec())
    );
}

#[test]
fn falls_through_to_lower_pack() {
    let manager = ResourceManager::new(vec![
        src("vanilla", &[("assets/minecraft/only_low.txt", b"low")]),
        src("user", &[("assets/minecraft/only_high.txt", b"high")]),
    ]);
    assert_eq!(
        manager.read("assets/minecraft/only_low.txt"),
        Some(b"low".to_vec())
    );
    assert_eq!(
        manager.read("assets/minecraft/only_high.txt"),
        Some(b"high".to_vec())
    );
}

#[test]
fn missing_resource_returns_none() {
    let manager = ResourceManager::new(vec![src("vanilla", &[])]);
    assert_eq!(manager.read("assets/minecraft/nope.txt"), None);
}

#[test]
fn asset_path_builds_namespaced_path() {
    let loc = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert_eq!(
        ResourceManager::asset_path(&loc, "blockstates", "json"),
        "assets/minecraft/blockstates/block/stone.json"
    );
    let custom = ResourceLocation::parse("mypack:foo/bar").unwrap();
    assert_eq!(
        ResourceManager::asset_path(&custom, "textures", "png"),
        "assets/mypack/textures/foo/bar.png"
    );
}

#[test]
fn read_asset_resolves_namespaced() {
    let manager = ResourceManager::new(vec![src(
        "vanilla",
        &[("assets/minecraft/textures/block/stone.png", b"stone")],
    )]);
    let loc = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert_eq!(
        manager.read_asset(&loc, "textures", "png"),
        Some(b"stone".to_vec())
    );
}

#[test]
fn list_dedupes_across_packs() {
    let manager = ResourceManager::new(vec![
        src(
            "vanilla",
            &[
                ("assets/minecraft/a.txt", b"v"),
                ("assets/minecraft/shared.txt", b"v"),
            ],
        ),
        src(
            "user",
            &[
                ("assets/minecraft/b.txt", b"u"),
                ("assets/minecraft/shared.txt", b"u"),
            ],
        ),
    ]);
    let mut listed = manager.list("assets/minecraft/");
    listed.sort();
    assert_eq!(
        listed,
        vec![
            "assets/minecraft/a.txt".to_string(),
            "assets/minecraft/b.txt".to_string(),
            "assets/minecraft/shared.txt".to_string(), // appears once despite being in both
        ]
    );
}

#[test]
fn push_adds_highest_priority() {
    let mut manager = ResourceManager::new(vec![src(
        "vanilla",
        &[("assets/minecraft/x.txt", b"vanilla")],
    )]);
    manager.push(src("user", &[("assets/minecraft/x.txt", b"user")]));
    assert_eq!(
        manager.read("assets/minecraft/x.txt"),
        Some(b"user".to_vec())
    );
    assert_eq!(manager.len(), 2);
}

#[test]
fn pack_meta_read_from_winning_pack() {
    let manager = ResourceManager::new(vec![
        src(
            "vanilla",
            &[(
                "pack.mcmeta",
                br#"{"pack":{"pack_format":1,"description":"vanilla"}}"#,
            )],
        ),
        src(
            "user",
            &[(
                "pack.mcmeta",
                br#"{"pack":{"pack_format":55,"description":"user"}}"#,
            )],
        ),
    ]);
    let meta = manager.read_pack_meta().unwrap();
    assert_eq!(meta.pack_format, 55);
    assert_eq!(meta.description.plain_text(), "user");
}

#[test]
fn pack_meta_missing_is_error() {
    let manager = ResourceManager::new(vec![src("empty", &[])]);
    assert!(manager.read_pack_meta().is_err());
}

#[test]
fn pack_meta_falls_back_to_version_json() {
    // Vanilla client.jar has NO root pack.mcmeta; metadata comes from version.json.
    let manager = ResourceManager::new(vec![src(
        "vanilla",
        &[(
            "version.json",
            br#"{"id":"26.2","pack_version":{"resource_major":88,"resource_minor":0,"data_major":107,"data_minor":1}}"#,
        )],
    )]);
    let meta = manager.read_pack_meta().unwrap();
    assert_eq!(meta.pack_format, 88);
    assert_eq!(meta.description.plain_text(), "26.2");
}

#[test]
fn pack_mcmeta_preferred_over_version_json() {
    let manager = ResourceManager::new(vec![src(
        "pack",
        &[
            (
                "pack.mcmeta",
                br#"{"pack":{"pack_format":42,"description":"user"}}"#,
            ),
            ("version.json", br#"{"id":"x","pack_version":88}"#),
        ],
    )]);
    let meta = manager.read_pack_meta().unwrap();
    assert_eq!(meta.pack_format, 42);
}

// --- G4: full vanilla-resource-pack compatibility, proven with a synthetic stack.
//
// The stated requirement is that vanilla's own assets are simply the lowest
// pack in the stack, and any user pack overrides on top of it with vanilla's
// exact override semantics. These tests build that stack explicitly.

use lodestone_assets::PackMeta;

/// A stand-in for the vanilla built-in pack: one texture, one model, one
/// blockstate, all in the `minecraft` namespace.
fn vanilla_pack() -> Box<dyn ResourceSource> {
    src(
        "vanilla",
        &[
            (
                "assets/minecraft/textures/block/stone.png",
                b"VANILLA_STONE_PNG",
            ),
            (
                "assets/minecraft/models/block/stone.json",
                b"{\"vanilla\":true}",
            ),
            (
                "assets/minecraft/blockstates/stone.json",
                b"{\"vanilla\":true}",
            ),
        ],
    )
}

#[test]
fn user_pack_overrides_single_texture_only() {
    // A pack that replaces exactly one texture must win for that texture and
    // leave everything else served by vanilla (the classic "retexture" pack).
    let manager = ResourceManager::new(vec![
        vanilla_pack(),
        src(
            "retexture",
            &[(
                "assets/minecraft/textures/block/stone.png",
                b"CUSTOM_STONE_PNG",
            )],
        ),
    ]);
    let stone = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert_eq!(
        manager.read_asset(&stone, "textures", "png"),
        Some(b"CUSTOM_STONE_PNG".to_vec()),
        "user texture must override vanilla"
    );
    // The model and blockstate were not touched, so vanilla still serves them.
    assert_eq!(
        manager.read_asset(&stone, "models", "json"),
        Some(b"{\"vanilla\":true}".to_vec()),
        "untouched model still served by vanilla"
    );
}

#[test]
fn user_pack_can_add_a_new_namespace() {
    // A pack introducing its own namespace resolves without disturbing vanilla.
    let manager = ResourceManager::new(vec![
        vanilla_pack(),
        src(
            "mypack",
            &[("assets/mypack/textures/block/widget.png", b"WIDGET")],
        ),
    ]);
    let widget = ResourceLocation::parse("mypack:block/widget").unwrap();
    assert_eq!(
        manager.read_asset(&widget, "textures", "png"),
        Some(b"WIDGET".to_vec()),
        "added namespace must resolve"
    );
    // Vanilla namespace is unaffected.
    let stone = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert!(manager.read_asset(&stone, "textures", "png").is_some());
}

#[test]
fn user_pack_overrides_a_model() {
    let manager = ResourceManager::new(vec![
        vanilla_pack(),
        src(
            "remodel",
            &[(
                "assets/minecraft/models/block/stone.json",
                b"{\"custom\":true}",
            )],
        ),
    ]);
    let stone = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert_eq!(
        manager.read_asset(&stone, "models", "json"),
        Some(b"{\"custom\":true}".to_vec()),
        "user model must override vanilla model"
    );
}

#[test]
fn three_pack_stack_resolves_by_priority() {
    // vanilla < middle < top. The most specific (top) wins where it defines a
    // resource; otherwise resolution falls through in priority order.
    let manager = ResourceManager::new(vec![
        vanilla_pack(),
        src(
            "middle",
            &[("assets/minecraft/textures/block/stone.png", b"MIDDLE")],
        ),
        src(
            "top",
            &[("assets/minecraft/textures/block/stone.png", b"TOP")],
        ),
    ]);
    let stone = ResourceLocation::parse("minecraft:block/stone").unwrap();
    assert_eq!(
        manager.read_asset(&stone, "textures", "png"),
        Some(b"TOP".to_vec())
    );
    // list() across the whole stack shows the overridden path exactly once.
    let listed = manager.list("assets/minecraft/textures/block/");
    let hits = listed
        .iter()
        .filter(|p| p.as_str() == "assets/minecraft/textures/block/stone.png")
        .count();
    assert_eq!(hits, 1, "overridden path must appear once in list()");
}

#[test]
fn pack_format_gating() {
    // Flat pack_format: accepted only for the exact host format.
    let flat = PackMeta::parse(br#"{"pack":{"pack_format":88,"description":"x"}}"#).unwrap();
    assert!(flat.accepts(88));
    assert!(!flat.accepts(87));

    // supported_formats range: accepted anywhere in the inclusive range.
    let ranged = PackMeta::parse(
        br#"{"pack":{"pack_format":80,"description":"x","supported_formats":[80,88]}}"#,
    )
    .unwrap();
    assert!(ranged.accepts(80));
    assert!(ranged.accepts(88));
    assert!(!ranged.accepts(79));
    assert!(!ranged.accepts(89));
}
