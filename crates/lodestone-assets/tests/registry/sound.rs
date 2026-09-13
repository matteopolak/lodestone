//! `sounds.json` sound-event registry: parsing, weighted selection, chaining.

use lodestone_assets::sound::{ResolvedSound, SoundKind, SoundRegistry};
use lodestone_assets::{ResourceLocation, SoundError};

fn parse(json: &str) -> Result<SoundRegistry, SoundError> {
    SoundRegistry::parse(json.as_bytes())
}

/// A deterministic "roll" source that always returns the same value.
fn fixed(value: u32) -> impl FnMut(u32) -> u32 {
    move |max| value.min(max.saturating_sub(1))
}

#[test]
fn parses_string_shorthand_with_defaults() {
    let reg = parse(r#"{"entity.pig.ambient":{"sounds":["mob/pig/say1"]}}"#).unwrap();
    let ev = reg.event("entity.pig.ambient").unwrap();
    assert_eq!(ev.sounds.len(), 1);
    let s = &ev.sounds[0];
    assert_eq!(s.name.to_string(), "minecraft:mob/pig/say1");
    assert_eq!(s.volume, 1.0);
    assert_eq!(s.pitch, 1.0);
    assert_eq!(s.weight, 1);
    assert_eq!(s.kind, SoundKind::File);
    assert!(!s.stream);
    assert!(!s.preload);
    assert_eq!(s.attenuation_distance, 16);
}

#[test]
fn parses_object_entry_all_fields() {
    let reg = parse(
        r#"{"music.menu":{"subtitle":"subtitles.music","sounds":[
        {"name":"music/menu/menu1","volume":0.5,"pitch":1.2,"weight":3,
         "stream":true,"preload":true,"attenuation_distance":32,"type":"file"}]}}"#,
    )
    .unwrap();
    let ev = reg.event("music.menu").unwrap();
    assert_eq!(ev.subtitle.as_deref(), Some("subtitles.music"));
    let s = &ev.sounds[0];
    assert_eq!(s.volume, 0.5);
    assert_eq!(s.pitch, 1.2);
    assert_eq!(s.weight, 3);
    assert!(s.stream);
    assert!(s.preload);
    assert_eq!(s.attenuation_distance, 32);
    assert_eq!(s.kind, SoundKind::File);
}

#[test]
fn rejects_non_positive_volume_and_weight() {
    assert!(matches!(
        parse(r#"{"e":{"sounds":[{"name":"a","volume":0}]}}"#),
        Err(SoundError::InvalidField(_))
    ));
    assert!(matches!(
        parse(r#"{"e":{"sounds":[{"name":"a","weight":0}]}}"#),
        Err(SoundError::InvalidField(_))
    ));
}

#[test]
fn rejects_unknown_type() {
    assert!(matches!(
        parse(r#"{"e":{"sounds":[{"name":"a","type":"sound"}]}}"#),
        Err(SoundError::UnknownType(_))
    ));
}

#[test]
fn malformed_json_is_rejected() {
    assert!(matches!(parse("not json"), Err(SoundError::Json(_))));
}

#[test]
fn event_count_and_names_are_exposed() {
    let reg = parse(r#"{"a":{"sounds":["x"]},"b":{"sounds":["y"]}}"#).unwrap();
    assert_eq!(reg.len(), 2);
    let mut names = reg.event_names().collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["a", "b"]);
}

#[test]
fn event_keys_validate_and_keep_custom_namespaces_distinct() {
    let reg = parse(
        r#"{
            "minecraft:base": {"sounds":["base"]},
            "mod:base": {"sounds":["mod-base"]}
        }"#,
    )
    .unwrap();

    // The default namespace is implicit in the built-in JSON shape, while the
    // custom key remains qualified rather than colliding with it.
    assert!(reg.event("base").is_some());
    assert!(reg.event("minecraft:base").is_some());
    assert!(reg.event("mod:base").is_some());
    let mut names = reg.event_names().collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["base", "mod:base"]);
    assert_eq!(reg.event("base").unwrap().sounds[0].name.to_string(), "minecraft:base");
    assert_eq!(reg.event("mod:base").unwrap().sounds[0].name.to_string(), "minecraft:mod-base");

    // A malformed raw key is rejected before it can become a map entry.
    assert!(matches!(
        parse(r#"{"bad key":{"sounds":["x"]}}"#),
        Err(SoundError::Location(_))
    ));
}

#[test]
fn weighted_selection_picks_by_cumulative_weight() {
    // weights 1 (a) then 3 (b): rolls 0 -> a, rolls 1..=3 -> b.
    let reg =
        parse(r#"{"e":{"sounds":[{"name":"a","weight":1},{"name":"b","weight":3}]}}"#).unwrap();
    let r0 = reg.resolve("e", &mut fixed(0)).unwrap().unwrap();
    assert_eq!(r0.file, ResourceLocation::parse("minecraft:a").unwrap());
    let r1 = reg.resolve("e", &mut fixed(1)).unwrap().unwrap();
    assert_eq!(r1.file, ResourceLocation::parse("minecraft:b").unwrap());
    let r3 = reg.resolve("e", &mut fixed(3)).unwrap().unwrap();
    assert_eq!(r3.file, ResourceLocation::parse("minecraft:b").unwrap());
    assert_eq!(reg.total_weight("e").unwrap(), 4);
}

#[test]
fn resolved_sound_carries_file_path() {
    let reg = parse(r#"{"e":{"sounds":["entity/creeper/say"]}}"#).unwrap();
    let r = reg.resolve("e", &mut fixed(0)).unwrap().unwrap();
    assert_eq!(
        r.file_path(),
        "assets/minecraft/sounds/entity/creeper/say.ogg"
    );
}

#[test]
fn type_event_chains_and_multiplies_volume_and_pitch() {
    // "parent" references "child" (an event) at volume 0.5; child plays file at 0.8.
    let reg = parse(
        r#"{
        "parent":{"sounds":[{"name":"child","type":"event","volume":0.5,"pitch":2.0}]},
        "child":{"sounds":[{"name":"snd/file","volume":0.8,"pitch":0.5}]}
    }"#,
    )
    .unwrap();
    let r: ResolvedSound = reg.resolve("parent", &mut fixed(0)).unwrap().unwrap();
    assert_eq!(
        r.file,
        ResourceLocation::parse("minecraft:snd/file").unwrap()
    );
    assert!((r.volume - 0.4).abs() < 1e-6, "volume {}", r.volume); // 0.5 * 0.8
    assert!((r.pitch - 1.0).abs() < 1e-6, "pitch {}", r.pitch); // 2.0 * 0.5
}

#[test]
fn event_type_weight_uses_referenced_event_total() {
    // A file entry (weight 1) beside an event entry whose referenced event
    // totals weight 5; parent total should be 1 + 5 = 6, not 1 + 1.
    let reg = parse(
        r#"{
        "parent":{"sounds":[
            {"name":"solo","weight":1},
            {"name":"child","type":"event"}
        ]},
        "child":{"sounds":[{"name":"c","weight":5}]}
    }"#,
    )
    .unwrap();
    assert_eq!(reg.total_weight("parent").unwrap(), 6);
}

#[test]
fn reference_cycle_is_bounded_not_infinite() {
    let reg = parse(
        r#"{
        "a":{"sounds":[{"name":"b","type":"event"}]},
        "b":{"sounds":[{"name":"a","type":"event"}]}
    }"#,
    )
    .unwrap();
    assert!(matches!(
        reg.resolve("a", &mut fixed(0)),
        Err(SoundError::ReferenceCycle { .. })
    ));
    assert!(matches!(
        reg.total_weight("a"),
        Err(SoundError::ReferenceCycle { .. })
    ));
}

#[test]
fn missing_event_resolves_to_none() {
    let reg = parse(r#"{"e":{"sounds":["x"]}}"#).unwrap();
    assert!(
        reg.resolve("does.not.exist", &mut fixed(0))
            .unwrap()
            .is_none()
    );
}

#[test]
fn replace_false_appends_and_true_resets_on_merge() {
    let base = parse(r#"{"e":{"sounds":["a"]}}"#).unwrap();
    // A second pack that appends (replace defaults false).
    let mut merged = base.clone();
    let add = parse(r#"{"e":{"sounds":["b"]}}"#).unwrap();
    merged.merge_from(&add);
    assert_eq!(merged.event("e").unwrap().sounds.len(), 2);
    // A pack that replaces resets the entry list.
    let mut merged2 = base.clone();
    let repl = parse(r#"{"e":{"replace":true,"sounds":["c"]}}"#).unwrap();
    merged2.merge_from(&repl);
    let ev = merged2.event("e").unwrap();
    assert_eq!(ev.sounds.len(), 1);
    assert_eq!(ev.sounds[0].name.to_string(), "minecraft:c");
}
