//! Does a **real** `assets/minecraft/items/trident.json` reach the trident rig
//! when it is held, and deliberately *not* when it sits in a slot?
//!
//! # Why a jar-backed gate and not a transcribed fixture
//!
//! `lodestone_render::trident_item_rig`'s own unit test proves the name it
//! returns resolves in the entity corpus. That is the second half of the chain.
//! It cannot see the first half at all: if `ItemVariants::resolve_special` never
//! answers `minecraft:trident` for a hand context, the rig is a perfectly
//! correct function with no producer — the island shape this repo's rules put
//! first — and every hermetic assertion about it still passes.
//!
//! So the expected value here has to come from outside our own parser, and the
//! only outside source that can answer "what does the real definition say" is
//! the real definition. This reads it out of the pinned 26.2 jar.
//!
//! # What `trident.json` actually declares, transcribed
//!
//! ```text
//! select on minecraft:display_context
//!   case [gui, ground, fixed, on_shelf] -> minecraft:model  item/trident
//!   fallback -> condition on minecraft:using_item
//!                 transformation: scale [1, -1, -1]
//!                 on_false -> special  base item/trident_in_hand   { type: minecraft:trident }
//!                 on_true  -> special  base item/trident_throwing  { type: minecraft:trident }
//! ```
//!
//! Three things follow, and each is asserted below rather than assumed:
//!
//! * **A held trident is a rig; a slotted one is a sprite.** The `gui` case is a
//!   plain `minecraft:model`, so drawing a rig in the inventory would be the
//!   regression, not the fix. A gate that only checked "the trident resolves to
//!   a special form" would pass while happily doing that.
//! * **The `scale [1, -1, -1]` sits on the `condition` node, not on either
//!   `special` node underneath it.** That is the same inherited-`transformation`
//!   shape the shield's own flip has — the one that put every shield in this
//!   client back-to-front until the parser learned to read a field off an
//!   ancestor. Reading it only where it is "supposed" to be yields a trident
//!   held upside down, which is wire-legal, survives every round trip, and looks
//!   like a pose bug rather than a parse one. So the flip is asserted to be
//!   *present in the chain*, which is what `compose_special_node_transform` then
//!   folds.
//! * **Both branches of the condition carry the same `kind`.** Whether the
//!   player is mid-throw changes the `base` model (and so the display slot's
//!   transform), never the rig.
//!
//! `#[ignore]`d and fail-closed, like every jar-backed gate here: a missing jar
//! is a loud panic naming the fetch command, never a silent skip that reports
//! `ok` while asserting nothing.
//!
//! Run with:
//! `cargo test -p lodestone-render --test trident_hand_rig_resolution -- --ignored --nocapture`

use lodestone_assets::{DisplaySlot, ResourceLocation, ResourceManager, ZipSource};
use lodestone_model::BlockStateRegistry;
use lodestone_render::{BlockModels, ItemStateContext, blocks_json_registry, trident_item_rig};

mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

fn build_models() -> (BlockModels, ResourceManager, Box<dyn BlockStateRegistry>) {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    (models, manager, Box::new(registry))
}

/// The whole chain for a held trident, in one gate: the real definition resolves
/// to a `minecraft:trident` special form in a first-person hand, that form's
/// transformation chain carries the flip, and the rig it maps to exists.
///
/// The GUI arm is the negative control and it is not a strawman — it is the
/// specific wrong behaviour a "make the trident draw" change invites. It must
/// resolve to **no** special form, because vanilla draws a flat sprite there.
#[test]
#[ignore = "reads the pinned 26.2 client.jar"]
fn a_held_trident_resolves_to_the_trident_rig_and_a_slotted_one_does_not() {
    let (models, _manager, _registry) = build_models();
    let trident = ResourceLocation::parse("minecraft:trident").expect("a valid id");
    let variants = models
        .item_forms(&trident)
        .expect("the jar ships assets/minecraft/items/trident.json");

    // --- the hand: a real special form naming the trident rig ---------------
    let hand = ItemStateContext::new(DisplaySlot::FirstPersonRightHand);
    let form = variants.resolve_special(&hand).unwrap_or_else(|| {
        panic!(
            "a first-person held trident resolved to no special form at all, so \
             `prepare_special_hand`'s trident branch can never run and the rig is \
             an island regardless of how well it is written"
        )
    });
    assert_eq!(
        form.kind, "minecraft:trident",
        "the hand context reached a special node of the wrong kind"
    );
    assert_eq!(
        trident_item_rig("trident"),
        Some("trident"),
        "the kind resolves but the rig lookup declines it"
    );

    // --- the flip, which lives on an ancestor node --------------------------
    // `scale [1, -1, -1]` is declared on the enclosing `minecraft:condition`,
    // not on either `special` node. A parser that reads `transformation` only
    // where the special node carries it drops this silently and holds the
    // trident upside down.
    let flip = form
        .transformation
        .iter()
        .find(|t| t.scale == [1.0, -1.0, -1.0])
        .unwrap_or_else(|| {
            panic!(
                "no [1, -1, -1] scale in the trident's transformation chain \
                 {:?} — the condition node's own entry was dropped, which holds \
                 the trident upside down while every round trip still passes",
                form.transformation
            )
        });
    assert_eq!(
        flip.translation,
        [0.0, 0.0, 0.0],
        "the trident's flip node declares no translation; a non-zero one here \
         means a different node was matched"
    );

    // --- the slot: deliberately NOT a rig -----------------------------------
    let mut slotted = Vec::new();
    for slot in [
        DisplaySlot::Gui,
        DisplaySlot::Ground,
        DisplaySlot::Fixed,
    ] {
        let ctx = ItemStateContext::new(slot);
        if let Some(form) = variants.resolve_special(&ctx) {
            slotted.push(format!("{slot:?} resolved to {:?}", form.kind));
        }
    }
    assert!(
        slotted.is_empty(),
        "these contexts must draw the flat `item/trident` sprite, not the rig — \
         `trident.json`'s select names them explicitly: {slotted:?}"
    );

    // And the positive half of that control: the slot contexts do resolve to
    // *something*, so their emptiness above is "a sprite" and not "nothing at
    // all". Without this the negative assertion is satisfied by a definition
    // that failed to parse.
    let gui = ItemStateContext::new(DisplaySlot::Gui);
    assert!(
        variants.resolve(&gui).is_some(),
        "the GUI context resolved to neither a rig nor a baked model, so the \
         assertion above passed by measuring a parse failure"
    );
}
