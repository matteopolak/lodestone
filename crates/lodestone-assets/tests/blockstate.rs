//! Tests for blockstate JSON parsing ([`BlockStates`]).

use lodestone_assets::{BlockStateDefinition, BlockStates, When, parse_variant_key};
use std::collections::BTreeMap;

fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn parses_simple_variant() {
    let json = br#"{"variants":{"":{"model":"minecraft:block/stone"}}}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Variants(variants) = &bs.definition else {
        panic!("expected variants");
    };
    assert_eq!(variants.len(), 1);
    let (key, models) = &variants[0];
    assert_eq!(key, "");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].model.to_string(), "minecraft:block/stone");
    assert_eq!(models[0].x, 0);
    assert_eq!(models[0].y, 0);
    assert!(!models[0].uvlock);
    assert_eq!(models[0].weight, 1);
}

#[test]
fn parses_weighted_variant_list() {
    let json = br#"{"variants":{"":[
        {"model":"minecraft:block/stone"},
        {"model":"minecraft:block/stone","y":180,"weight":3}
    ]}}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Variants(variants) = &bs.definition else {
        panic!("expected variants");
    };
    let (_, models) = &variants[0];
    assert_eq!(models.len(), 2);
    assert_eq!(models[1].y, 180);
    assert_eq!(models[1].weight, 3);
}

#[test]
fn parses_variant_rotation_and_uvlock() {
    let json = br#"{"variants":{"facing=east":{"model":"m:x","x":90,"y":270,"uvlock":true}}}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Variants(variants) = &bs.definition else {
        panic!()
    };
    let (_, models) = &variants[0];
    assert_eq!(
        (models[0].x, models[0].y, models[0].uvlock),
        (90, 270, true)
    );
}

#[test]
fn variant_key_parsing() {
    let map = parse_variant_key("facing=north,half=top");
    assert_eq!(map.get("facing").map(String::as_str), Some("north"));
    assert_eq!(map.get("half").map(String::as_str), Some("top"));
    assert!(parse_variant_key("").is_empty());
}

#[test]
fn selects_matching_variant() {
    let json = br#"{"variants":{
        "snowy=false":{"model":"m:a"},
        "snowy=true":{"model":"m:b"}
    }}"#;
    let bs = BlockStates::parse(json).unwrap();
    let picked = bs.select_variant(&props(&[("snowy", "true")])).unwrap();
    assert_eq!(picked[0].model.to_string(), "m:b");
    let picked = bs.select_variant(&props(&[("snowy", "false")])).unwrap();
    assert_eq!(picked[0].model.to_string(), "m:a");
}

#[test]
fn parses_multipart_with_apply_list_and_no_when() {
    let json = br#"{"multipart":[
        {"apply":{"model":"minecraft:block/oak_fence_post"}},
        {"apply":{"model":"minecraft:block/oak_fence_side","uvlock":true},"when":{"north":"true"}}
    ]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Multipart(cases) = &bs.definition else {
        panic!("expected multipart");
    };
    assert_eq!(cases.len(), 2);
    assert!(cases[0].when.is_none()); // always applies
    assert!(cases[1].when.is_some());
}

#[test]
fn parses_multipart_apply_as_weighted_list() {
    let json = br#"{"multipart":[
        {"apply":[{"model":"m:a"},{"model":"m:b"}],"when":{"north":"true"}}
    ]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Multipart(cases) = &bs.definition else {
        panic!()
    };
    assert_eq!(cases[0].apply.len(), 2);
}

#[test]
fn multipart_when_implicit_and() {
    // Multiple keys in a when are ANDed together.
    let json =
        br#"{"multipart":[{"apply":{"model":"m:a"},"when":{"facing":"north","powered":"false"}}]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Multipart(cases) = &bs.definition else {
        panic!()
    };
    let when = cases[0].when.as_ref().unwrap();
    assert!(when.matches(&props(&[("facing", "north"), ("powered", "false")])));
    assert!(!when.matches(&props(&[("facing", "north"), ("powered", "true")])));
    assert!(!when.matches(&props(&[("facing", "south"), ("powered", "false")])));
}

#[test]
fn multipart_when_explicit_or() {
    let json = br#"{"multipart":[{"apply":{"model":"m:a"},"when":{"OR":[{"north":"true"},{"south":"true"}]}}]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Multipart(cases) = &bs.definition else {
        panic!()
    };
    let when = cases[0].when.as_ref().unwrap();
    assert!(when.matches(&props(&[("north", "true"), ("south", "false")])));
    assert!(when.matches(&props(&[("north", "false"), ("south", "true")])));
    assert!(!when.matches(&props(&[("north", "false"), ("south", "false")])));
}

#[test]
fn multipart_when_explicit_and() {
    let json = br#"{"multipart":[{"apply":{"model":"m:a"},"when":{"AND":[{"facing":"north"},{"powered":"false"}]}}]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Multipart(cases) = &bs.definition else {
        panic!()
    };
    let when = cases[0].when.as_ref().unwrap();
    assert!(when.matches(&props(&[("facing", "north"), ("powered", "false")])));
    assert!(!when.matches(&props(&[("facing", "north"), ("powered", "true")])));
}

#[test]
fn multipart_when_pipe_alternatives() {
    // segment_amount: "2|3" matches either value.
    let json = br#"{"multipart":[{"apply":{"model":"m:a"},"when":{"segment_amount":"2|3"}}]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let BlockStateDefinition::Multipart(cases) = &bs.definition else {
        panic!()
    };
    let when = cases[0].when.as_ref().unwrap();
    assert!(when.matches(&props(&[("segment_amount", "2")])));
    assert!(when.matches(&props(&[("segment_amount", "3")])));
    assert!(!when.matches(&props(&[("segment_amount", "1")])));
}

#[test]
fn direct_when_construction_matches() {
    let when = When::Match {
        property: "facing".into(),
        values: vec!["north".into(), "south".into()],
    };
    assert!(when.matches(&props(&[("facing", "south")])));
    assert!(!when.matches(&props(&[("facing", "east")])));
}

#[test]
fn model_refs_iterates_all_referenced_models() {
    let json = br#"{"multipart":[
        {"apply":{"model":"m:a"}},
        {"apply":[{"model":"m:b"},{"model":"m:c"}],"when":{"x":"1"}}
    ]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let mut refs: Vec<String> = bs.model_refs().map(|r| r.model.to_string()).collect();
    refs.sort();
    assert_eq!(refs, vec!["m:a", "m:b", "m:c"]);
}

#[test]
fn malformed_blockstate_errors() {
    assert!(BlockStates::parse(b"not json").is_err());
    assert!(BlockStates::parse(br#"{}"#).is_err()); // neither variants nor multipart
    assert!(BlockStates::parse(br#"{"variants":{"":{"model":123}}}"#).is_err());
    // invalid model identifier
    assert!(BlockStates::parse(br#"{"variants":{"":{"model":"BAD NS"}}}"#).is_err());
}

#[test]
fn applicable_models_variants_returns_single_matching_group() {
    let json = br#"{"variants":{
        "facing=north":[{"model":"m:n"},{"model":"m:n2"}],
        "facing=south":{"model":"m:s"}
    }}"#;
    let bs = BlockStates::parse(json).unwrap();
    let groups = bs.applicable_models(&props(&[("facing", "north")]));
    assert_eq!(groups.len(), 1);
    let names: Vec<String> = groups[0].iter().map(|r| r.model.to_string()).collect();
    assert_eq!(names, vec!["m:n", "m:n2"]);
    // No matching variant -> no groups.
    assert!(
        bs.applicable_models(&props(&[("facing", "west")]))
            .is_empty()
    );
}

#[test]
fn applicable_models_multipart_unions_matching_cases() {
    let json = br#"{"multipart":[
        {"apply":{"model":"m:base"}},
        {"apply":{"model":"m:north"},"when":{"north":"true"}},
        {"apply":{"model":"m:east"},"when":{"east":"true"}}
    ]}"#;
    let bs = BlockStates::parse(json).unwrap();
    let groups = bs.applicable_models(&props(&[("north", "true"), ("east", "false")]));
    // base (no when) + north (matches); east excluded.
    let mut names: Vec<String> = groups.iter().map(|g| g[0].model.to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["m:base", "m:north"]);
}
