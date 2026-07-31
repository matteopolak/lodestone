//! Hermetic tests for protocol 776 item-stack **data-component** decoding.
//!
//! The wire shape of a non-empty stack (26.2 `ItemStack.OPTIONAL_STREAM_CODEC`)
//! is `count VarInt`, `item id VarInt`, then a `DataComponentPatch`:
//! `added VarInt`, `removed VarInt`, then the added components as
//! `(type id VarInt, payload)` pairs and the removed components as bare
//! `type id VarInt`s. The added components are **not** length-prefixed, so an
//! unmodeled component cannot be skipped in place — these tests pin both the
//! decode of the components we model and the graceful degradation when an
//! unmodeled component appears.
//!
//! Golden bytes are hand-built from the 26.2 spec, **plus** two payloads
//! captured verbatim from the real 26.2 server and replayed here with no server
//! attached (see `replays_the_captured_*` below).
//!
//! Hand-built bytes alone are not enough, and this file is the proof: the
//! `minecraft:tool` test and the decoder both wrote a `HolderSet`'s direct
//! holders as `registry id + 1`, and the pair round-tripped green while the
//! server's actual encoding is the bare id. Only the capture caught it. Any new
//! component whose shape is inferred rather than observed should get a captured
//! fixture too.

use lodestone_core::{Nbt, Writer, write_network_nbt};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemEnchantment, Text, ToolBlocks, ToolPatch,
    VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_data::data_component_types::component_type_name;
use lodestone_data::items::item_id;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// Resolves a data-component-type id from its canonical name via the generated
/// table, so the test never hardcodes a numeric component id.
fn component_id(name: &str) -> i32 {
    (0..)
        .find(|&id| component_type_name(id) == Some(name))
        .expect("known component type")
}

fn handle(id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, id, payload)
        .expect("handle packet")
}

/// Builds a `container_set_slot` payload (window 1, state 1, slot 36) wrapping a
/// single item stack whose raw component-patch bytes are `patch`.
fn set_slot_with_patch(item: &str, count: i32, patch: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(1); // window id
    w.var_i32(1); // state id
    w.i16(36); // slot
    w.var_i32(count); // stack count (> 0 -> present)
    w.var_i32(item_id(item).expect("known item"));
    w.bytes(patch);
    w.into_vec()
}

fn slot_item(directives: &[Directive]) -> lodestone_model::ItemStack {
    match directives {
        [Directive::Emit(ClientEvent::ContainerSlot { item, .. })] => {
            item.clone().expect("present item")
        }
        other => panic!("expected a single ContainerSlot emit, got {other:?}"),
    }
}

/// A diamond pickaxe with a custom name, durability damage, and one enchantment
/// decodes into the modeled component fields.
#[test]
fn decodes_modeled_components() {
    let mut patch = Writer::default();
    patch.var_i32(3); // three added components
    patch.var_i32(0); // none removed

    // custom_name: a network-NBT text component (here a bare string tag).
    patch.var_i32(component_id("minecraft:custom_name"));
    write_network_nbt(&mut patch, &Nbt::String("Digger".to_owned())).unwrap();

    // damage: a single VarInt.
    patch.var_i32(component_id("minecraft:damage"));
    patch.var_i32(137);

    // enchantments: a VarInt map of Holder<Enchantment> (id + 1) -> VarInt level.
    patch.var_i32(component_id("minecraft:enchantments"));
    patch.var_i32(1); // one entry
    patch.var_i32(12 + 1); // enchantment registry id 12, holder-encoded
    patch.var_i32(4); // level IV

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(item.count, 1);
    assert_eq!(item.components.damage, Some(137));
    assert_eq!(
        item.components.custom_name.as_ref().map(Text::to_plain_string),
        Some("Digger".to_owned())
    );
    assert_eq!(
        item.components.enchantments,
        vec![ItemEnchantment { id: 12, level: 4 }]
    );
    assert!(!item.components.has_unmodeled);
}

/// A stack carrying a component this build does not model still decodes: the
/// session survives, the item/count are intact, and the stack is flagged as
/// carrying unmodeled components rather than raising a fatal decode error.
#[test]
fn tolerates_an_unmodeled_component() {
    // `minecraft:custom_data` (id 0) is an NBT blob this build does not model.
    let mut patch = Writer::default();
    patch.var_i32(1); // one added component
    patch.var_i32(0); // none removed
    patch.var_i32(component_id("minecraft:custom_data"));
    write_network_nbt(
        &mut patch,
        &Nbt::Compound(vec![("x".to_owned(), Nbt::Int(1))]),
    )
    .unwrap();

    let payload = set_slot_with_patch("minecraft:stone", 5, patch.as_slice());
    // Must not error out the whole packet handling.
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.item.to_string(), "minecraft:stone");
    assert_eq!(item.count, 5);
    assert!(item.components.has_unmodeled);
}

/// A `minecraft:tool` on the wire decodes rule-for-rule, in order, including
/// both `HolderSet<Block>` shapes and the independently-optional speed and
/// correct-for-drops fields.
///
/// Wire shape (26.2 `Tool.STREAM_CODEC`): a VarInt-counted rule list, then an
/// f32 default speed, a VarInt damage-per-block, and a bool
/// can-destroy-in-creative. Each rule is a `HolderSet<Block>` — VarInt `0` then
/// an identifier for a tag, else `n + 1` followed by `n` **bare** VarInt registry
/// ids — then `optional(f32)` and `optional(bool)`, each a present-flag byte
/// followed by the value.
///
/// # Only the set size is offset by one
///
/// This test first wrote each holder as `registry id + 1`, by analogy with
/// `ByteBufCodecs.holder`, and passed — because the decoder had made the same
/// assumption and the two cancelled. `holderSet` uses `holderRegistry` instead,
/// which writes the id **as-is**; the captured
/// `tests/fixtures/tool_component_explicit.hex` spells `minecraft:stone`
/// (registry 1) as `01`, not `02`. The bytes below are now the server's shape,
/// and [`replays_the_captured_tool_component_fixture`] pins the same decode
/// against the capture itself so a symmetric drift cannot recur.
#[test]
fn decodes_a_tool_component() {
    let mut patch = Writer::default();
    patch.var_i32(1); // one added component
    patch.var_i32(0); // none removed
    patch.var_i32(component_id("minecraft:tool"));

    patch.var_i32(2); // two rules

    // Rule 1: a tag-backed set that denies drops and supplies no speed —
    // vanilla's `#incorrect_for_<material>_tool` shape.
    patch.var_i32(0); // HolderSet discriminator 0 = named tag
    patch.string("minecraft:incorrect_for_diamond_tool");
    patch.bool(false); // no speed
    patch.bool(true); // has correct_for_drops...
    patch.bool(false); // ...and it is false

    // Rule 2: an explicit two-block set that supplies a speed and no verdict.
    patch.var_i32(2 + 1); // HolderSet discriminator = size + 1 (the *only* +1 here)
    patch.var_i32(1); // block registry id 1 (minecraft:stone), written as-is
    patch.var_i32(193); // block registry id 193 (minecraft:obsidian), as-is
    patch.bool(true);
    patch.f32(12.5);
    patch.bool(false); // no correct_for_drops

    patch.f32(1.0); // default_mining_speed
    patch.var_i32(3); // damage_per_block
    patch.bool(true); // can_destroy_blocks_in_creative

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(
        !item.components.has_unmodeled,
        "minecraft:tool is modeled now, so nothing may be flagged partial"
    );
    let ToolPatch::Set(tool) = &item.components.tool else {
        panic!("expected a set tool component, got {:?}", item.components.tool);
    };
    assert_eq!(tool.default_mining_speed(), 1.0);
    assert_eq!(tool.damage_per_block, 3);
    assert!(tool.can_destroy_blocks_in_creative);
    assert_eq!(tool.rules.len(), 2);

    assert_eq!(
        tool.rules[0].blocks,
        ToolBlocks::Tag(
            "minecraft:incorrect_for_diamond_tool"
                .parse()
                .expect("tag key")
        ),
        "a tag is written without its leading `#`"
    );
    assert_eq!(tool.rules[0].speed(), None);
    assert_eq!(tool.rules[0].correct_for_drops, Some(false));

    assert_eq!(
        tool.rules[1].blocks,
        ToolBlocks::Blocks(vec![1, 193]),
        "explicit holders are bare registry ids; only the set size is offset"
    );
    assert_eq!(tool.rules[1].speed(), Some(12.5));
    assert_eq!(tool.rules[1].correct_for_drops, None);
}

/// A stack whose patch *removes* `minecraft:tool` is distinguishable from one
/// that never mentioned it. Vanilla clears the component to nothing, not to the
/// item's prototype, so a removed tool mines like a bare hand — collapsing the
/// two would leave a `/give …[!minecraft:tool]` pickaxe at full 8x speed.
#[test]
fn a_removed_tool_component_is_distinct_from_an_absent_one() {
    let mut patch = Writer::default();
    patch.var_i32(0); // nothing added
    patch.var_i32(1); // one removed
    patch.var_i32(component_id("minecraft:tool"));

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));
    assert_eq!(item.components.tool, ToolPatch::Removed);
    assert!(!item.components.has_unmodeled);

    // The control: an empty patch on the same item must *not* read as a removal.
    let mut empty = Writer::default();
    empty.var_i32(0);
    empty.var_i32(0);
    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, empty.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));
    assert_eq!(
        item.components.tool,
        ToolPatch::Inherited,
        "an empty patch inherits the item's prototype tool; reading it as a \
         removal would make every real pickaxe mine like a fist"
    );
}

// ---------------------------------------------------------------------------
// Server-authored bytes, replayed with no server attached
// ---------------------------------------------------------------------------

/// Parses one of the captured hex-text fixtures under `tests/fixtures/`:
/// `#`-prefixed provenance/annotation lines, then whitespace-separated hex
/// bytes. Same format `tests/live_tool_component.rs` writes.
fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    let bytes: Vec<u8> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(|token| u8::from_str_radix(token, 16).expect("fixture hex byte"))
        .collect();
    assert!(!bytes.is_empty(), "fixture {name} carried no bytes");
    bytes
}

/// The `minecraft:tool` decode, against bytes the **real 26.2 server** wrote.
///
/// `tests/live_tool_component.rs` captures these and asserts the same fields,
/// but that file is behind both `--features live-tool` and `#[ignore]`, so
/// without this test the captured bytes are read by nothing the default suite
/// runs — the fixture would sit in the tree as decoration. Replaying it here is
/// what makes the hand-built sibling above non-circular: if the two ever
/// disagree about the wire shape, the server's copy wins.
#[test]
fn replays_the_captured_tool_component_fixture() {
    let payload = fixture_bytes("tool_component_explicit.hex");
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.item.to_string(), "minecraft:diamond_pickaxe");
    assert!(!item.components.has_unmodeled);
    let ToolPatch::Set(tool) = &item.components.tool else {
        panic!("expected a set tool component, got {:?}", item.components.tool);
    };
    assert_eq!(tool.default_mining_speed(), 1.5);
    assert_eq!(tool.damage_per_block, 3);
    assert!(tool.can_destroy_blocks_in_creative);
    assert_eq!(tool.rules.len(), 2);
    assert_eq!(
        tool.rules[0].blocks,
        ToolBlocks::Tag(
            "minecraft:incorrect_for_diamond_tool"
                .parse()
                .expect("tag key")
        )
    );
    assert_eq!(tool.rules[0].speed(), None);
    assert_eq!(tool.rules[0].correct_for_drops, Some(false));
    assert_eq!(
        tool.rules[1].blocks,
        ToolBlocks::Blocks(vec![1, 193]),
        "the server wrote `01` and `c1 01` for registry ids 1 and 193 — bare, \
         not offset by one"
    );
    assert_eq!(tool.rules[1].speed(), Some(12.5));
    assert_eq!(tool.rules[1].correct_for_drops, None);
}

/// The finding that shaped the design, replayed from the capture: a stock
/// diamond pickaxe's whole component patch is `00 00`. There is no
/// `minecraft:tool` on the wire, so no amount of decoding can make a pickaxe dig
/// faster — the per-item prototype census in `src/generated/tools.rs` is what
/// does.
///
/// This asserts an **absence**, and its control is
/// [`replays_the_captured_tool_component_fixture`]: same decoder, same field,
/// same assertion shape, with a tool actually present. A detector stuck at
/// `Inherited` would fail that one.
#[test]
fn replays_the_captured_plain_pickaxe_fixture() {
    let payload = fixture_bytes("tool_component_absent_plain_pickaxe.hex");
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(item.count, 1);
    assert!(
        !item.components.has_unmodeled,
        "a stock pickaxe carries no components at all"
    );
    assert_eq!(
        item.components.tool,
        ToolPatch::Inherited,
        "a stock pickaxe sends no minecraft:tool"
    );
}

/// Modeled components decoded *before* an unmodeled one are retained.
#[test]
fn retains_modeled_components_before_an_unmodeled_one() {
    let mut patch = Writer::default();
    patch.var_i32(2); // two added components
    patch.var_i32(0);
    // A modeled component first...
    patch.var_i32(component_id("minecraft:damage"));
    patch.var_i32(42);
    // ...then an unmodeled one.
    patch.var_i32(component_id("minecraft:custom_data"));
    write_network_nbt(&mut patch, &Nbt::Compound(Vec::new())).unwrap();

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.components.damage, Some(42));
    assert!(item.components.has_unmodeled);
}
