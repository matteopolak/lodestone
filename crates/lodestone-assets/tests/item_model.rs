//! Hermetic tests for the 1.21.4+ item definition tree.
//!
//! The fixtures are the *real* vanilla JSON shapes (bow, elytra, crossbow,
//! compass, player_head) transcribed from the 26.2 jar — an authority I did not
//! invent — and the resolution cases pick property values that force a wrong
//! implementation to diverge (a naive "first entry wins" range dispatch, or a
//! select that ignores the fallback, produces a different model than asserted).

use lodestone_assets::{
    ItemModel, ItemModelNode, ItemModelOutput, ItemPropertyContext, ResourceLocation,
};

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

/// A scripted context: fixed answers per property, so resolution is deterministic
/// and a divergent implementation must produce a different output.
#[derive(Default)]
struct Ctx {
    conditions: std::collections::HashMap<String, bool>,
    selects: std::collections::HashMap<String, String>,
    ranges: std::collections::HashMap<String, f32>,
}
impl ItemPropertyContext for Ctx {
    fn condition(&self, property: &str, _component: Option<&str>) -> bool {
        self.conditions.get(property).copied().unwrap_or(false)
    }
    fn select(&self, property: &str) -> Option<String> {
        self.selects.get(property).cloned()
    }
    fn range(&self, property: &str) -> f32 {
        self.ranges.get(property).copied().unwrap_or(0.0)
    }
}

#[test]
fn parses_plain_model_leaf() {
    let m = ItemModel::parse(
        br#"{"model":{"type":"minecraft:model","model":"minecraft:block/stone"}}"#,
    )
    .unwrap();
    assert_eq!(
        m.root,
        ItemModelNode::Model {
            model: loc("minecraft:block/stone"),
            tints: vec![],
        }
    );
    assert_eq!(m.model_refs(), vec![&loc("minecraft:block/stone")]);
}

const BOW: &[u8] = br#"{
  "model": { "type": "minecraft:condition", "property": "minecraft:using_item",
    "on_false": { "type": "minecraft:model", "model": "minecraft:item/bow" },
    "on_true": { "type": "minecraft:range_dispatch", "property": "minecraft:use_duration", "scale": 0.05,
      "entries": [
        { "threshold": 0.9, "model": { "type": "minecraft:model", "model": "minecraft:item/bow_pulling_2" } },
        { "threshold": 0.65, "model": { "type": "minecraft:model", "model": "minecraft:item/bow_pulling_1" } }
      ],
      "fallback": { "type": "minecraft:model", "model": "minecraft:item/bow_pulling_0" } } } }"#;

#[test]
fn bow_enumerates_every_reachable_model() {
    let m = ItemModel::parse(BOW).unwrap();
    let refs: std::collections::BTreeSet<String> =
        m.model_refs().iter().map(|r| r.to_string()).collect();
    assert_eq!(
        refs,
        [
            "minecraft:item/bow",
            "minecraft:item/bow_pulling_0",
            "minecraft:item/bow_pulling_1",
            "minecraft:item/bow_pulling_2"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    );
}

#[test]
fn bow_range_dispatch_picks_greatest_threshold_not_exceeding_value() {
    let m = ItemModel::parse(BOW).unwrap();
    // using_item = true routes into the range dispatch; use_duration * 0.05 = value.
    let resolve_at = |duration: f32| {
        let mut ctx = Ctx::default();
        ctx.conditions.insert("minecraft:using_item".into(), true);
        ctx.ranges.insert("minecraft:use_duration".into(), duration);
        match m.resolve(&ctx).as_slice() {
            [ItemModelOutput::Model { model, .. }] => model.to_string(),
            other => panic!("expected one model, got {other:?}"),
        }
    };
    // value 0.0 -> below 0.65 -> fallback pulling_0 (a "first entry wins" bug would say pulling_2).
    assert_eq!(resolve_at(0.0), "minecraft:item/bow_pulling_0");
    // value = 13*0.05 = 0.65 -> exactly the 0.65 threshold -> pulling_1.
    assert_eq!(resolve_at(13.0), "minecraft:item/bow_pulling_1");
    // value = 17*0.05 = 0.85 -> still < 0.9 -> pulling_1 (a naive round-up would jump to _2).
    assert_eq!(resolve_at(17.0), "minecraft:item/bow_pulling_1");
    // value = 18*0.05 = 0.9 -> reaches 0.9 -> pulling_2.
    assert_eq!(resolve_at(18.0), "minecraft:item/bow_pulling_2");
    // using_item = false -> the plain bow, ignoring the dispatch entirely.
    let ctx = Ctx::default();
    assert_eq!(
        m.resolve(&ctx),
        vec![ItemModelOutput::Model {
            model: &loc("minecraft:item/bow"),
            tints: &[]
        }]
    );
}

const CROSSBOW_SELECT: &[u8] = br#"{
  "model": { "type": "minecraft:select", "property": "minecraft:charge_type",
    "cases": [
      { "when": "arrow", "model": { "type": "minecraft:model", "model": "minecraft:item/crossbow_arrow" } },
      { "when": ["rocket","firework"], "model": { "type": "minecraft:model", "model": "minecraft:item/crossbow_firework" } }
    ],
    "fallback": { "type": "minecraft:model", "model": "minecraft:item/crossbow" } } }"#;

#[test]
fn select_matches_case_list_and_falls_back() {
    let m = ItemModel::parse(CROSSBOW_SELECT).unwrap();
    let pick = |key: Option<&str>| {
        let mut ctx = Ctx::default();
        if let Some(k) = key {
            ctx.selects.insert("minecraft:charge_type".into(), k.into());
        }
        match m.resolve(&ctx).as_slice() {
            [ItemModelOutput::Model { model, .. }] => model.to_string(),
            other => panic!("expected one model, got {other:?}"),
        }
    };
    assert_eq!(pick(Some("arrow")), "minecraft:item/crossbow_arrow");
    // "firework" is the SECOND string of the second case's list — a single-string
    // matcher would miss it and wrongly fall back.
    assert_eq!(pick(Some("firework")), "minecraft:item/crossbow_firework");
    assert_eq!(pick(Some("rocket")), "minecraft:item/crossbow_firework");
    // unknown / unset -> fallback.
    assert_eq!(pick(Some("nonsense")), "minecraft:item/crossbow");
    assert_eq!(pick(None), "minecraft:item/crossbow");
}

#[test]
fn special_node_surfaces_renderer_kind_and_base() {
    let json = br#"{"model":{"type":"minecraft:special",
        "base":"minecraft:item/template_skull",
        "model":{"type":"minecraft:player_head"}}}"#;
    let m = ItemModel::parse(json).unwrap();
    assert_eq!(
        m.root,
        ItemModelNode::Special {
            base: loc("minecraft:item/template_skull"),
            kind: "minecraft:player_head".into(),
            transformation: Vec::new(),
        }
    );
    // The data-vs-code seam: the special renderer is enumerable as (base, kind).
    assert_eq!(
        m.special_renderers(),
        vec![(
            &loc("minecraft:item/template_skull"),
            "minecraft:player_head"
        )]
    );
    // Its base is still a real model the sprite atlas must carry.
    assert_eq!(m.model_refs(), vec![&loc("minecraft:item/template_skull")]);
    // Resolving yields a Special output for the renderer to dispatch on.
    let ctx = Ctx::default();
    assert_eq!(
        m.resolve(&ctx),
        vec![ItemModelOutput::Special {
            base: &loc("minecraft:item/template_skull"),
            kind: "minecraft:player_head",
            transformation: &[],
        }]
    );
}

/// The skull family's real gap: a `special` node's own `"transformation"`
/// field (issue's own JSON, `assets/minecraft/items/skeleton_skull.json`) is
/// parsed rather than silently dropped.
#[test]
fn special_node_carries_its_own_transformation() {
    let json = br#"{"model":{"type":"minecraft:special",
        "base":"minecraft:item/template_skull",
        "model":{"type":"minecraft:head","kind":"skeleton"},
        "transformation":{
            "left_rotation":[1.0,0.0,0.0,-0.0],
            "right_rotation":[0.0,0.0,0.0,1.0],
            "scale":[1.0,1.0,1.0],
            "translation":[0.5,0.0,0.5]
        }}}"#;
    let m = ItemModel::parse(json).unwrap();
    let ItemModelNode::Special { transformation, .. } = &m.root else {
        panic!("expected a special node");
    };
    let [t] = transformation.as_slice() else {
        panic!("the skull family carries exactly one node transformation");
    };
    assert_eq!(t.translation, [0.5, 0.0, 0.5]);
    assert_eq!(t.left_rotation, [1.0, 0.0, 0.0, -0.0]);
    assert_eq!(t.scale, [1.0, 1.0, 1.0]);
    assert_eq!(t.right_rotation, [0.0, 0.0, 0.0, 1.0]);

    // Resolving still surfaces it on the output, not just the parsed node —
    // this is what a caller actually reads.
    let ctx = Ctx::default();
    let resolved = m.resolve(&ctx);
    let [ItemModelOutput::Special { transformation, .. }] = resolved.as_slice() else {
        panic!("expected one special output");
    };
    assert_eq!(transformation[0].translation, [0.5, 0.0, 0.5]);
}

/// An **ancestor** node's `"transformation"` reaches the `special` node under
/// it. This is `assets/minecraft/items/shield.json`'s exact shape, trimmed to
/// the branch under test: the `scale [1, -1, -1]` that vanilla's
/// `ShieldSpecialRenderer` used to apply in code is hoisted into the item
/// definition and sits on the enclosing `minecraft:condition` node, **not** on
/// the `minecraft:special` node.
///
/// Reading only the `special` node's own field dropped it, and the pixel-level
/// consequence is unusually invisible: a shield drawn without the `z` flip
/// shows the *back* of its plate, whose UV region is byte-identical between
/// `entity/shield/shield_base.png` and `shield_base_nopattern.png` (they differ
/// in exactly 200 texels, all inside the front-face rect), and over which every
/// loom mask is fully transparent. So a dyed, a patterned and a plain shield
/// all render bit-identical and every pattern layer moves zero pixels, with
/// every draw call correctly issued.
///
/// The expected values come from the shipped jar file, not from this parser.
#[test]
fn an_ancestor_nodes_transformation_reaches_the_special_node_under_it() {
    let json = br#"{"model":{
        "type":"minecraft:condition",
        "property":"minecraft:using_item",
        "on_false":{"type":"minecraft:special",
            "base":"minecraft:item/shield",
            "model":{"type":"minecraft:shield"}},
        "on_true":{"type":"minecraft:special",
            "base":"minecraft:item/shield_blocking",
            "model":{"type":"minecraft:shield"}},
        "transformation":{
            "left_rotation":[0.0,0.0,0.0,1.0],
            "right_rotation":[0.0,0.0,0.0,1.0],
            "scale":[1.0,-1.0,-1.0],
            "translation":[0.0,0.0,0.0]
        }}}"#;
    let m = ItemModel::parse(json).unwrap();

    // Both branches inherit it — the accumulation is static, exactly as
    // vanilla's `bake` passes one composed matrix to `onTrue` and `onFalse`.
    let ctx = Ctx::default();
    let resolved = m.resolve(&ctx);
    let [ItemModelOutput::Special { transformation, .. }] = resolved.as_slice() else {
        panic!("expected one special output, got {resolved:?}");
    };
    assert_eq!(
        transformation.len(),
        1,
        "the condition node's transformation must reach the special node's chain"
    );
    assert_eq!(transformation[0].scale, [1.0, -1.0, -1.0]);
    assert_eq!(transformation[0].translation, [0.0, 0.0, 0.0]);

    // `outputs()` walks every branch, so both `on_true` and `on_false` must
    // carry it — a fix that only threaded the resolved branch would pass the
    // assertion above and leave the blocking pose flipped the wrong way.
    let all = m.outputs();
    assert_eq!(all.len(), 2, "both branches are special nodes: {all:?}");
    for output in &all {
        let ItemModelOutput::Special { transformation, .. } = output else {
            panic!("expected a special output, got {output:?}");
        };
        assert_eq!(transformation.len(), 1, "branch missing the chain: {output:?}");
        assert_eq!(transformation[0].scale, [1.0, -1.0, -1.0]);
    }
}

/// A node's own `"transformation"` composes **after** its ancestors', so the
/// chain is ordered outermost-first — the order
/// `compose_special_node_transform` folds it in, and the one vanilla's
/// repeated `Transformation.compose(parent, this.transformation)` produces.
///
/// No shipped 26.2 item nests two (measured: 77 `special` nodes with exactly
/// one on the path, 14 with none, zero with two), so this is the case only a
/// resource pack reaches — and the case a design that stored a single
/// `Option` could not represent at all.
#[test]
fn nested_transformations_accumulate_outermost_first() {
    let json = br#"{"model":{
        "type":"minecraft:condition",
        "property":"minecraft:using_item",
        "on_false":{"type":"minecraft:special",
            "base":"minecraft:item/shield",
            "model":{"type":"minecraft:shield"},
            "transformation":{
                "left_rotation":[0.0,0.0,0.0,1.0],
                "right_rotation":[0.0,0.0,0.0,1.0],
                "scale":[2.0,2.0,2.0],
                "translation":[9.0,0.0,0.0]
            }},
        "on_true":{"type":"minecraft:empty"},
        "transformation":{
            "left_rotation":[0.0,0.0,0.0,1.0],
            "right_rotation":[0.0,0.0,0.0,1.0],
            "scale":[1.0,-1.0,-1.0],
            "translation":[0.0,3.0,0.0]
        }}}"#;
    let m = ItemModel::parse(json).unwrap();
    let ItemModelNode::Condition { on_false, .. } = &m.root else {
        panic!("expected a condition root");
    };
    let ItemModelNode::Special { transformation, .. } = on_false.as_ref() else {
        panic!("expected a special under on_false");
    };
    assert_eq!(transformation.len(), 2, "{transformation:?}");
    // Outermost (the condition) first, then the node's own. The two differ on
    // every component, so a reversed chain cannot satisfy this by accident.
    assert_eq!(transformation[0].scale, [1.0, -1.0, -1.0]);
    assert_eq!(transformation[0].translation, [0.0, 3.0, 0.0]);
    assert_eq!(transformation[1].scale, [2.0, 2.0, 2.0]);
    assert_eq!(transformation[1].translation, [9.0, 0.0, 0.0]);
}

/// A `special` node whose JSON omits `"transformation"` entirely, and whose
/// ancestors carry none either (e.g. `chest.json`), parses to an **empty
/// chain** rather than to a one-entry chain holding `Transformation::default()`
/// — the caller must be able to tell "compose nothing" from "compose the
/// identity", even though the two matrices are equal, because only the empty
/// case matches vanilla's `Optional::isEmpty` short-circuit in
/// `Transformation.compose`.
#[test]
fn special_node_with_no_transformation_field_parses_to_an_empty_chain() {
    let json = br#"{"model":{"type":"minecraft:special",
        "base":"minecraft:item/template_chest",
        "model":{"type":"minecraft:chest","texture":"minecraft:normal"}}}"#;
    let m = ItemModel::parse(json).unwrap();
    let ItemModelNode::Special { transformation, .. } = &m.root else {
        panic!("expected a special node");
    };
    assert!(
        transformation.is_empty(),
        "expected an empty chain, got {transformation:?}"
    );
}

#[test]
fn composite_renders_every_submodel() {
    let json = br#"{"model":{"type":"minecraft:composite","models":[
        {"type":"minecraft:model","model":"minecraft:item/a"},
        {"type":"minecraft:model","model":"minecraft:item/b"}]}}"#;
    let m = ItemModel::parse(json).unwrap();
    let ctx = Ctx::default();
    assert_eq!(
        m.resolve(&ctx),
        vec![
            ItemModelOutput::Model {
                model: &loc("minecraft:item/a"),
                tints: &[]
            },
            ItemModelOutput::Model {
                model: &loc("minecraft:item/b"),
                tints: &[]
            },
        ]
    );
}

#[test]
fn tints_are_captured_on_model_leaves() {
    let json = br#"{"model":{"type":"minecraft:model","model":"minecraft:item/leather_boots",
        "tints":[{"type":"minecraft:dye","default":-6265536}]}}"#;
    let m = ItemModel::parse(json).unwrap();
    let ItemModelNode::Model { tints, .. } = &m.root else {
        panic!("expected model leaf")
    };
    assert_eq!(tints.len(), 1);
    assert_eq!(tints[0].kind, "minecraft:dye");
    assert_eq!(tints[0].default, Some(-6265536));
}

#[test]
fn unknown_node_type_is_preserved_not_rejected() {
    // A newer pack node this parser doesn't model must not fail the whole file.
    let json = br#"{"model":{"type":"minecraft:some_future_selector","weird":true}}"#;
    let m = ItemModel::parse(json).unwrap();
    assert_eq!(
        m.root,
        ItemModelNode::Other {
            kind: "some_future_selector".into()
        }
    );
    assert!(m.model_refs().is_empty());
    assert!(m.resolve(&Ctx::default()).is_empty());
}

#[test]
fn empty_node_renders_nothing() {
    let m = ItemModel::parse(br#"{"model":{"type":"minecraft:empty"}}"#).unwrap();
    assert_eq!(m.root, ItemModelNode::Empty);
    assert!(m.resolve(&Ctx::default()).is_empty());
}

#[test]
fn missing_model_field_is_a_clear_error_not_a_panic() {
    assert!(ItemModel::parse(br#"{"foo":1}"#).is_err());
    assert!(ItemModel::parse(br#"not json"#).is_err());
    assert!(ItemModel::parse(br#"{"model":{"type":"minecraft:model"}}"#).is_err());
}
