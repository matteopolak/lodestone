//! Acceptance gate for the **item variant axis**: that an item's several baked
//! forms exist, that they are genuinely different geometry, and that a live
//! context picks the right one at the right tick.
//!
//! # Why this gate had to be new, and why the existing ones are not controls
//!
//! `first_person_hand_light_pixels` and `thrown_and_held_item_pixels` both draw a
//! held item and both pass **unchanged** by this work — because every pose and
//! every variant is the identity at `using == false`, which is the only state
//! either of them can produce. Their green says nothing about whether a drawn bow
//! or an in-hand spyglass resolves correctly. This gate drives `using` **true**
//! and asserts the *crossings*, not "the model changed".
//!
//! # What it proves that a hermetic test cannot
//!
//! `item_render`'s unit tests already assert the crossings over a transcribed
//! `bow.json` fixture. They cannot see the thing that actually breaks:
//! `item/bow_pulling_0/1/2` are `item/generated` **sprite** models, so their
//! geometry is walked out of the alpha outline of a texture that must be *in the
//! stitched atlas*. Before this work those three textures were in no atlas at all —
//! reachable from no blockstate and from no *baked* item model — so a resolver that
//! picked `bow_pulling_2` perfectly would still have drawn nothing. Only
//! `BlockModels::build` over the real jar can show that the pre-stitch discovery
//! pass reached them.
//!
//! `#[ignore]`d and fail-closed, like the sibling gates. Run with:
//! `cargo test -p lodestone-render --test item_variant_gate -- --ignored --nocapture`

use lodestone_assets::{DisplaySlot, ResourceLocation, ResourceManager, ZipSource};
use lodestone_render::{
    BlockModels, ItemGeometry, ItemStateContext, ItemVariants, blocks_json_registry,
    entity::{Arm, hand_transform},
};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

fn build_models() -> BlockModels {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    BlockModels::build(&manager, &registry).expect("bake block models")
}

fn loc(s: &str) -> ResourceLocation {
    s.parse().expect("valid resource location")
}

fn forms<'a>(models: &'a BlockModels, item: &str) -> &'a ItemVariants {
    models
        .item_forms(&loc(item))
        .unwrap_or_else(|| panic!("{item} has no baked geometry at all"))
}

/// The bow bakes four distinct forms and a live context crosses between them at
/// exactly ticks 13 and 18.
///
/// The crossings are **predicted from Mojang's own numbers**, not measured and
/// then written down: `items/bow.json` carries `scale: 0.05` with thresholds
/// `0.65` and `0.9`, so `0.65 / 0.05 = 13` and `0.9 / 0.05 = 18`.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn the_bow_bakes_four_forms_and_crosses_at_thirteen_and_eighteen() {
    let models = build_models();
    let bow = forms(&models, "minecraft:bow");

    // All four models baked, each with real geometry. This is the atlas half: a
    // `bow_pulling_*` whose `layer0` never stitched would be `Some` with zero
    // quads under a naive implementation, or absent under this one — assert
    // non-empty so neither passes.
    let wanted = [
        "minecraft:item/bow",
        "minecraft:item/bow_pulling_0",
        "minecraft:item/bow_pulling_1",
        "minecraft:item/bow_pulling_2",
    ];
    eprintln!("=== minecraft:bow variants ({}) ===", bow.variant_count());
    for model in wanted {
        let geometry = bow
            .variant(&loc(model))
            .unwrap_or_else(|| panic!("{model} did not bake — is its layer0 in the atlas?"));
        eprintln!("  {model}: {} quads", geometry.quads.len());
        assert!(
            !geometry.quads.is_empty(),
            "{model} baked zero quads: its sprite is not in the stitched atlas, so the \
             pre-stitch discovery pass did not reach it"
        );
    }
    assert_eq!(bow.variant_count(), 4, "bow.json names exactly four models");

    // The crossings, through the real resolver and the real live context.
    let at = |ticks: u32| {
        let ctx = ItemStateContext::new(DisplaySlot::FirstPersonRightHand).with_use(true, ticks);
        bow.resolve_ref(&ctx)
            .unwrap_or_else(|| panic!("bow resolved to no model at {ticks} ticks"))
            .to_string()
    };
    for ticks in [0, 1, 12] {
        assert_eq!(at(ticks), "minecraft:item/bow_pulling_0", "at {ticks} ticks");
    }
    for ticks in [13, 17] {
        assert_eq!(at(ticks), "minecraft:item/bow_pulling_1", "at {ticks} ticks");
    }
    for ticks in [18, 20, 72_000] {
        assert_eq!(at(ticks), "minecraft:item/bow_pulling_2", "at {ticks} ticks");
    }
    // Not using: the slack bow, which is also what `BlockModels::item` returns.
    let resting = ItemStateContext::new(DisplaySlot::FirstPersonRightHand);
    assert_eq!(
        bow.resolve_ref(&resting).map(|m| m.to_string()),
        Some("minecraft:item/bow".to_owned())
    );

    // The variants must be *different geometry*, not four aliases of one sprite.
    // A UV comparison rather than a quad count: all four are one-layer generated
    // models of the same 16x16 sprite size, so their quad counts can coincide
    // while their atlas rects cannot.
    let uv_of = |model: &str| {
        let g = bow.variant(&loc(model)).expect("baked");
        g.quads.first().map(|q| q.uvs[0]).expect("a baked quad")
    };
    let slack = uv_of("minecraft:item/bow");
    for model in [
        "minecraft:item/bow_pulling_0",
        "minecraft:item/bow_pulling_1",
        "minecraft:item/bow_pulling_2",
    ] {
        assert_ne!(
            uv_of(model), slack,
            "{model} samples the same atlas rect as item/bow — the variants are aliases, \
             so nothing visibly changes as the bow is drawn"
        );
    }

    // --- The negative control, and it needs no neuter ---------------------
    //
    // The path this work replaced is still present, as `BlockModels::item`: one
    // geometry per item id, resolved at load against the static GUI context. So
    // the *same detector* — the first quad's atlas rect — can be pointed at it,
    // and it must be unable to tell a drawn bow from a slack one however many
    // ticks are fed in, because there is nowhere to feed them.
    //
    // Watching this comparison come out **equal** is what makes the `assert_ne!`s
    // above evidence rather than a coincidence: it demonstrates the detector is
    // measuring the variant axis and not some incidental difference that a
    // flattened build would have shown too.
    let flat = models
        .item(&loc("minecraft:bow"))
        .expect("the inventory form still bakes");
    let flat_uv = flat.quads.first().map(|q| q.uvs[0]).expect("a baked quad");
    eprintln!("  control: BlockModels::item(bow) rect {flat_uv:?} vs item/bow {slack:?}");
    assert_eq!(
        flat_uv, slack,
        "the flattened accessor no longer returns the resting bow, so the control below \
         proves nothing"
    );
    // ...and it is *not* any of the drawn forms. The old path could therefore
    // never have produced the crossings asserted above, at any tick.
    for model in [
        "minecraft:item/bow_pulling_0",
        "minecraft:item/bow_pulling_1",
        "minecraft:item/bow_pulling_2",
    ] {
        assert_ne!(
            flat_uv,
            uv_of(model),
            "BlockModels::item(bow) returned {model}: the inventory accessor is supposed to \
             be the resting form specifically"
        );
    }
}

/// The step-1 case, and the one with no live state at all: 26 items name a
/// different model in the hand than in the inventory slot, and the **transform**
/// has to follow the variant too.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn the_spyglass_is_a_different_model_and_a_different_pose_in_the_hand() {
    let models = build_models();
    let spyglass = forms(&models, "minecraft:spyglass");

    let gui = spyglass
        .resolve_ref(&ItemStateContext::new(DisplaySlot::Gui))
        .expect("a gui variant");
    let hand = spyglass
        .resolve_ref(&ItemStateContext::new(DisplaySlot::FirstPersonRightHand))
        .expect("a first-person variant");
    assert_eq!(gui.to_string(), "minecraft:item/spyglass");
    assert_eq!(hand.to_string(), "minecraft:item/spyglass_in_hand");

    let gui_geometry = spyglass.gui().expect("the gui form bakes");
    let hand_geometry = spyglass
        .resolve(&ItemStateContext::new(DisplaySlot::FirstPersonRightHand))
        .expect("the in-hand form bakes");
    eprintln!(
        "=== spyglass: gui {} quads ({gui}), hand {} quads ({hand}) ===",
        gui_geometry.quads.len(),
        hand_geometry.quads.len()
    );

    // `item/spyglass_in_hand` is two authored cuboids — 11 faces — where
    // `item/spyglass` is an extruded sprite slab. The counts cannot coincide, and
    // the geometry assertion is what proves the hand draws the tube rather than
    // the flat sprite.
    assert!(
        !hand_geometry.quads.is_empty(),
        "the in-hand spyglass baked nothing — its #spyglass texture \
         (item/spyglass_model) reaches no blockstate, so only the variant \
         discovery pass can seed it into the atlas"
    );
    assert_ne!(
        hand_geometry.quads.len(),
        gui_geometry.quads.len(),
        "the hand and the slot resolved to the same geometry: the display_context \
         branch was flattened"
    );

    // **And the transform, not just the geometry.** `spyglass_in_hand` authors no
    // `firstperson_righthand` at all, so vanilla poses it with the identity;
    // `item/spyglass`'s chain (via `item/generated`) declares `[0, -90, 25]` at
    // scale 0.68. Taking the icon-level display map handed the tube the *sprite's*
    // pose, which is a plausible-looking wrong angle rather than an absence.
    let hand_transform_of =
        |g: &ItemGeometry| hand_transform(&g.display, Arm::Right, true);
    let posed_hand = hand_transform_of(hand_geometry);
    let posed_gui = hand_transform_of(gui_geometry);
    eprintln!("  hand firstperson transform: {posed_hand:?}");
    eprintln!("  slot firstperson transform: {posed_gui:?}");
    assert_ne!(
        posed_hand, posed_gui,
        "the in-hand variant reported the same firstperson_righthand transform as the \
         inventory sprite, so ItemGeometry::display is still the icon's map and not the \
         resolved model's"
    );
    assert_eq!(
        posed_hand.rotation,
        [0.0, 0.0, 0.0],
        "item/spyglass_in_hand declares no firstperson_righthand, so vanilla's \
         ItemTransform.NO_TRANSFORM applies — a non-zero rotation here means the \
         transform was inherited from item/generated"
    );
}

/// The axis is real across the whole pack, and it did not cost the flat majority
/// their geometry.
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn the_pack_bakes_more_variants_than_items() {
    let models = build_models();
    let items = models.item_count();
    let variants = models.item_variant_count();
    let branching: Vec<(String, usize)> = {
        let mut v: Vec<(String, usize)> = models
            .item_forms_iter()
            .filter(|(_, f)| f.variant_count() > 1)
            .map(|(id, f)| (id.to_string(), f.variant_count()))
            .collect();
        v.sort();
        v
    };
    eprintln!("=== items {items}, baked variants {variants} ===");
    // The cost figure `docs/item-variants.md` quotes, printed rather than
    // asserted: seeding every variant's textures is what widens the atlas, and a
    // number in prose with no run behind it is the staleness this repo keeps
    // paying for.
    eprintln!("atlas sprites: {}", models.atlas().sprites().len());
    eprintln!("items with more than one variant: {}", branching.len());
    for (id, n) in &branching {
        eprintln!("  {id}: {n}");
    }
    for m in models.item_bake_misses() {
        eprintln!("  miss/note: {m}");
    }

    // The pre-existing invariant, unchanged: the flat majority still bakes.
    assert!(
        items > 1400,
        "the variant axis must not cost any item its geometry, got {items}"
    );
    // And the new one: the 80 branch-carrying items measured over the real jar
    // must now show up as *extra* baked forms rather than as one flattened each.
    assert!(
        variants > items,
        "every item baked exactly one variant ({variants} for {items} items) — the \
         discovery pass is still resolving one context instead of enumerating outputs"
    );
    assert!(
        branching.len() >= 40,
        "only {} items bake more than one variant; 84 of 26.2's items do (measured), and a \
         branch node, and a `select` whose cases all name one model is the only \
         legitimate way that count falls",
        branching.len()
    );
    // Every recorded entry is a known note, not a bake failure.
    for m in models.item_bake_misses() {
        assert!(
            m.contains("composite icon") || m.contains("none of which stitched"),
            "unexpected item bake failure: {m}"
        );
    }
}

/// The bed family's real gap, over the real jar rather than a synthetic
/// fixture: `items/black_bed.json` composites `block/black_bed_head` (no
/// `"transformation"`) with `block/black_bed_foot` (`translation [0, 0, 1]`),
/// and both must now carry their own `node_transformation` through
/// `BlockModels::build`'s bake — the field `collect_item_variants`'s own doc
/// used to say `IconPart::Model` dropped outright.
///
/// This is the *data* half only: it proves the value reaches the baked
/// `ItemGeometry`, not that anything yet draws the head and foot together as
/// one composite icon (see that doc's own "composite-render boundary").
#[test]
#[ignore = "requires a fetched vanilla client.jar and generated/reports/blocks.json"]
fn a_beds_foot_submodel_bakes_with_its_own_node_transformation_and_the_head_does_not() {
    let models = build_models();
    let bed = forms(&models, "minecraft:black_bed");

    let head = bed
        .variant(&loc("minecraft:block/black_bed_head"))
        .expect("black_bed_head baked");
    let foot = bed
        .variant(&loc("minecraft:block/black_bed_foot"))
        .expect("black_bed_foot baked");

    eprintln!("=== minecraft:black_bed composite parts ===");
    eprintln!("  head: node_transformation = {:?}", head.node_transformation);
    eprintln!("  foot: node_transformation = {:?}", foot.node_transformation);

    assert!(
        head.node_transformation.is_empty(),
        "black_bed_head carries no \"transformation\" of its own in the real jar, so its \
         baked chain must be empty; got {:?}",
        head.node_transformation
    );
    assert_eq!(
        foot.node_transformation.len(),
        1,
        "black_bed_foot carries exactly one \"transformation\" in the real jar; got {:?}",
        foot.node_transformation
    );
    assert_eq!(
        foot.node_transformation[0].translation,
        [0.0, 0.0, 1.0],
        "the real jar's own literal — the offset that keeps the foot from z-fighting the head"
    );
}
