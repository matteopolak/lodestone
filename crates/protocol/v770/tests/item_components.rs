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
    ClientEvent, ConnectionState, Directive, EquipmentSlot, ItemEnchantment, Text, ToolBlocks,
    ToolPatch, VersionAdapter,
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

/// A component this build deliberately does **not** decode, for every gate that
/// needs to exercise the unmodeled-component path.
///
/// Every such gate used to name `minecraft:custom_data`, and when that got
/// modeled all six of them went green while asserting the opposite of what they
/// were written for — the *world* species of vacuous test: the source was
/// exemplary and the input stopped containing the structure under test.
/// [`the_unmodeled_stand_in_is_still_unmodeled`] is the control that fails
/// loudly, naming this constant, if this one is ever modeled too.
///
/// `minecraft:profile` was this stand-in until it was modeled (the player-head
/// owner identity a container full of skulls needs — see
/// `lodestone_model::ItemProfile`), which is exactly the failure mode this
/// comment warned about: it went quietly green under the *old* wrong assertion
/// until [`the_unmodeled_stand_in_is_still_unmodeled`] caught it by name.
///
/// `minecraft:instrument` is the replacement, chosen for the same property
/// `profile` had: `Instrument.STREAM_CODEC` is `ByteBufCodecs.holder` over a
/// `DIRECT_STREAM_CODEC` that is itself a nested holder (`SoundEvent`) plus two
/// floats plus a full chat component — genuinely expensive, so it is unlikely
/// to be modeled on a whim and quietly void these gates again.
const UNMODELED_COMPONENT: &str = "minecraft:instrument";

/// One added component, unmodeled, with an arbitrary payload behind it.
///
/// The payload bytes are never interpreted — decoding stops at the type id — so
/// their shape is irrelevant; what matters is that bytes follow, so a decoder
/// that wrongly consumed them would land somewhere plausible.
fn unmodeled_patch() -> Vec<u8> {
    let mut patch = Writer::default();
    patch.var_i32(1); // one added component
    patch.var_i32(0); // none removed
    patch.var_i32(component_id(UNMODELED_COMPONENT));
    write_network_nbt(
        &mut patch,
        &Nbt::Compound(vec![("x".to_owned(), Nbt::Int(1))]),
    )
    .unwrap();
    patch.into_vec()
}

/// The detector control for [`UNMODELED_COMPONENT`]: the stand-in must really be
/// unmodeled, or every gate built on it proves nothing.
#[test]
fn the_unmodeled_stand_in_is_still_unmodeled() {
    let payload = set_slot_with_patch("minecraft:stone", 1, &unmodeled_patch());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));
    assert!(
        item.components.has_unmodeled,
        "{UNMODELED_COMPONENT} is now decoded, so every gate using it as the \
         unmodeled stand-in has gone vacuous. Pick another one for \
         UNMODELED_COMPONENT — not a component you just modeled."
    );
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

    // enchantments: a VarInt map of Holder<Enchantment> -> VarInt level. The
    // key is a *bare* registry id, not the offset-by-one form
    // `minecraft:instrument`'s holder uses elsewhere in this crate — see
    // `read_enchantments`'s own doc, and `docs/item-data-component-decode.md`,
    // for why.
    patch.var_i32(component_id("minecraft:enchantments"));
    patch.var_i32(1); // one entry
    patch.var_i32(12); // enchantment registry id 12, bare
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
    let payload = set_slot_with_patch("minecraft:stone", 5, &unmodeled_patch());
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

/// `minecraft:pot_decorations` decodes its four sherds into the right four faces.
///
/// Wire shape (26.2 `PotDecorations.STREAM_CODEC` =
/// `ByteBufCodecs.registry(Registries.ITEM).apply(ByteBufCodecs.list(4))`): a
/// VarInt element count then that many **bare** item registry ids. The record's
/// field order is `back`, `left`, `right`, `front`.
///
/// # Why four *distinct* sherds
///
/// Four adjacent same-typed VarInt fields transpose byte-perfectly through any
/// codec, so four identical ids — or even four in a symmetric arrangement —
/// would pass under any permutation of the four faces. Each face gets a
/// different sherd, and each assertion names the face, so a transposition fails
/// and says which pair swapped.
#[test]
fn decodes_pot_decorations_into_the_right_four_faces() {
    let mut patch = Writer::default();
    patch.var_i32(1); // one added component
    patch.var_i32(0); // none removed
    patch.var_i32(component_id("minecraft:pot_decorations"));
    patch.var_i32(4); // ByteBufCodecs.list(4) element count
    for sherd in [
        "minecraft:angler_pottery_sherd",  // back
        "minecraft:blade_pottery_sherd",   // left
        "minecraft:howl_pottery_sherd",    // right
        "minecraft:snort_pottery_sherd",   // front
    ] {
        // `ByteBufCodecs.registry` is `idMapper`: the bare id, no `+1` and no
        // `0` sentinel. `minecraft:trim`'s holders two arms over *are* offset,
        // which is exactly the confusion this spells out.
        patch.var_i32(item_id(sherd).expect("known sherd item"));
    }

    let payload = set_slot_with_patch("minecraft:decorated_pot", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(
        !item.components.has_unmodeled,
        "minecraft:pot_decorations is modeled now, so nothing may be flagged partial"
    );
    let pot = item
        .components
        .pot_decorations
        .as_ref()
        .expect("a decorated pot's sherds");
    // Collected rather than asserted one-at-a-time: an `assert_eq!` per face
    // aborts at the first mismatch, so a transposition would only ever report
    // one half of the swap.
    let got = [
        pot.back.as_ref().map(ToString::to_string),
        pot.left.as_ref().map(ToString::to_string),
        pot.right.as_ref().map(ToString::to_string),
        pot.front.as_ref().map(ToString::to_string),
    ];
    let want = [
        Some("minecraft:angler_pottery_sherd".to_owned()),
        Some("minecraft:blade_pottery_sherd".to_owned()),
        Some("minecraft:howl_pottery_sherd".to_owned()),
        Some("minecraft:snort_pottery_sherd".to_owned()),
    ];
    let mismatches: Vec<String> = ["back", "left", "right", "front"]
        .iter()
        .zip(got.iter().zip(want.iter()))
        .filter(|(_, (g, w))| g != w)
        .map(|(face, (g, w))| format!("{face}: got {g:?}, want {w:?}"))
        .collect();
    assert!(
        mismatches.is_empty(),
        "pot_decorations faces decoded wrong: {mismatches:?}"
    );
}

/// Appends `minecraft:profile`'s wire payload (`ResolvableProfile.STREAM_CODEC`)
/// to `patch`: the identity half — either a full `GameProfile` (`name`/`id`
/// both `Some`) or a `Partial` (either independently `None`) — followed by an
/// always-present, four-field `PlayerSkin.Patch` tail.
///
/// `model_slim` is `None` for "no model override" and `Some(slim)` for one —
/// exercising the double-optional trap (`PlayerModelType.STREAM_CODEC.apply
/// (optional)`, a presence bool wrapping another bool) at least once, since
/// getting that one wrong misaligns nothing *inside* this component (there is
/// nothing after it to misread within the same field) but would misread the
/// very next component's type id as this field's second bool.
fn write_profile(
    patch: &mut Writer,
    name: Option<&str>,
    id: Option<uuid::Uuid>,
    properties: &[(&str, &str, Option<&str>)],
    body_texture: Option<&str>,
    model_slim: Option<bool>,
) {
    match (name, id) {
        (Some(name), Some(id)) => {
            patch.bool(true); // full GameProfile
            patch.uuid(id);
            patch.string(name);
        }
        _ => {
            patch.bool(false); // Partial
            match name {
                Some(n) => {
                    patch.bool(true);
                    patch.string(n);
                }
                None => patch.bool(false),
            }
            match id {
                Some(i) => {
                    patch.bool(true);
                    patch.uuid(i);
                }
                None => patch.bool(false),
            }
        }
    }
    patch.var_i32(i32::try_from(properties.len()).expect("property count"));
    for (prop_name, value, signature) in properties {
        patch.string(prop_name);
        patch.string(value);
        match signature {
            Some(sig) => {
                patch.bool(true);
                patch.string(sig);
            }
            None => patch.bool(false),
        }
    }
    // PlayerSkin.Patch: body/cape/elytra optional Identifiers, then an
    // optional PlayerModelType.
    match body_texture {
        Some(t) => {
            patch.bool(true);
            patch.string(t);
        }
        None => patch.bool(false),
    }
    patch.bool(false); // cape: absent
    patch.bool(false); // elytra: absent
    match model_slim {
        Some(slim) => {
            patch.bool(true);
            patch.bool(slim);
        }
        None => patch.bool(false),
    }
}

/// A full `GameProfile` form of `minecraft:profile` (uuid, name and properties
/// all present) decodes into [`lodestone_model::ItemProfile`], including a
/// signed `minecraft:textures` property — the field the skin resolver actually
/// needs, and the one this component existed to carry.
#[test]
fn decodes_a_full_profile_with_signed_textures() {
    let id = uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff);
    let mut patch = Writer::default();
    patch.var_i32(1); // one added component
    patch.var_i32(0); // none removed
    patch.var_i32(component_id("minecraft:profile"));
    write_profile(
        &mut patch,
        Some("Notch"),
        Some(id),
        &[("textures", "eyJ0ZXh0dXJlcyI6e319", Some("sig-bytes"))],
        None,
        None,
    );

    let payload = set_slot_with_patch("minecraft:player_head", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(
        !item.components.has_unmodeled,
        "minecraft:profile is modeled now, so nothing may be flagged partial"
    );
    let profile = item.components.profile.as_ref().expect("a player head's profile");
    assert_eq!(profile.name.as_deref(), Some("Notch"));
    assert_eq!(profile.id, Some(id));
    assert_eq!(profile.properties.len(), 1);
    assert_eq!(profile.properties[0].name, "textures");
    assert_eq!(profile.properties[0].value, "eyJ0ZXh0dXJlcyI6e319");
    assert_eq!(profile.properties[0].signature.as_deref(), Some("sig-bytes"));
}

/// The `Partial` form: a head placed with only a name (no uuid resolved yet, no
/// properties) still decodes, and `id`/`properties` read as genuinely absent
/// rather than defaulted to something that looks plausible. The skin-patch tail
/// also carries a real body-texture override and a model flag here, exercising
/// the double-optional trap [`write_profile`]'s doc describes.
#[test]
fn a_partial_profile_by_name_only_still_decodes_and_keeps_the_slot_after_it_aligned() {
    let mut patch = Writer::default();
    patch.var_i32(1);
    patch.var_i32(0);
    patch.var_i32(component_id("minecraft:profile"));
    write_profile(
        &mut patch,
        Some("Dinnerbone"),
        None,
        &[],
        Some("minecraft:custom/skin"),
        Some(true),
    );

    let payload = set_slot_with_patch("minecraft:player_head", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(!item.components.has_unmodeled);
    let profile = item.components.profile.as_ref().expect("a player head's profile");
    assert_eq!(profile.name.as_deref(), Some("Dinnerbone"));
    assert_eq!(profile.id, None, "a partial profile with no uuid must decode to None, not a guess");
    assert!(profile.properties.is_empty());
}

/// The whole point of modeling this component: a player head is no longer a
/// decode cliff for whatever comes after it in the same container. Three
/// pairwise-distinct slots (a compass, the head, a diamond sword with distinct
/// counts) — before this fix, the sword slot was silently lost the moment a
/// server sent a player head with an owner.
#[test]
fn a_player_head_with_a_profile_no_longer_truncates_the_container() {
    let mut head_patch = Writer::default();
    head_patch.var_i32(1);
    head_patch.var_i32(0);
    head_patch.var_i32(component_id("minecraft:profile"));
    write_profile(
        &mut head_patch,
        Some("Notch"),
        Some(uuid::Uuid::from_u128(0x0699_a79f_444e_9472_6a5b_efca_90e3_8aaf)),
        &[("textures", "eyJ0ZXh0dXJlcyI6e319", None)],
        None,
        None,
    );

    let payload = set_content(&[
        ("minecraft:compass", 1, empty_patch()),
        ("minecraft:player_head", 7, head_patch.into_vec()),
        ("minecraft:diamond_sword", 3, empty_patch()),
    ]);

    let directives = handle(play::clientbound::CONTAINER_SET_CONTENT, &payload);
    let ClientEvent::ContainerContent { items, .. } = expect_single_emit(&directives) else {
        panic!("wrong event");
    };
    let names: Vec<_> = items
        .iter()
        .map(|slot| slot.as_ref().map(|s| (s.item.to_string(), s.count)))
        .collect();
    assert_eq!(
        names,
        vec![
            Some(("minecraft:compass".to_owned(), 1)),
            Some(("minecraft:player_head".to_owned(), 7)),
            Some(("minecraft:diamond_sword".to_owned(), 3)),
        ],
        "all three slots must survive a player head carrying a profile"
    );
    let head = items[1].as_ref().expect("the head slot");
    assert!(!head.components.has_unmodeled);
    assert_eq!(
        head.components.profile.as_ref().and_then(|p| p.name.as_deref()),
        Some("Notch")
    );
}

/// `minecraft:brick` on a face means an *undecorated* face, and a short list's
/// missing tail means the same.
///
/// Both are `PotDecorations::getItem`: `item == Items.BRICK ?
/// Optional.empty() : Optional.of(item)`, and `i >= sherds.size()` for the tail.
/// A vanilla server always writes four elements (`ordered()` builds a
/// four-element list unconditionally), so the short form is the case only a
/// hand-built payload reaches — which is why it is pinned here rather than left
/// to a capture.
///
/// The discriminating part is that the *decorated* face still lands in the right
/// slot in both arms: a decoder that mapped brick to `Some` would put a sherd in
/// `left` here, and one that ignored the count would read past the payload.
#[test]
fn a_brick_face_and_a_short_list_both_decode_as_undecorated() {
    // Arm 1: an explicit four-element list whose back and right faces are bricks.
    let mut patch = Writer::default();
    patch.var_i32(1);
    patch.var_i32(0);
    patch.var_i32(component_id("minecraft:pot_decorations"));
    patch.var_i32(4);
    for sherd in [
        "minecraft:brick",                // back  -> None
        "minecraft:prize_pottery_sherd",  // left
        "minecraft:brick",                // right -> None
        "minecraft:skull_pottery_sherd",  // front
    ] {
        patch.var_i32(item_id(sherd).expect("known item"));
    }
    let payload = set_slot_with_patch("minecraft:decorated_pot", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));
    let pot = item.components.pot_decorations.expect("sherds");
    assert!(!item.components.has_unmodeled);
    assert_eq!(pot.back, None, "a brick back face is an undecorated one");
    assert_eq!(
        pot.left.as_ref().map(ToString::to_string),
        Some("minecraft:prize_pottery_sherd".to_owned())
    );
    assert_eq!(pot.right, None, "a brick right face is an undecorated one");
    assert_eq!(
        pot.front.as_ref().map(ToString::to_string),
        Some("minecraft:skull_pottery_sherd".to_owned())
    );

    // Arm 2: a two-element list. `back` and `left` are read; `right` and `front`
    // are the absent tail.
    let mut short = Writer::default();
    short.var_i32(1);
    short.var_i32(0);
    short.var_i32(component_id("minecraft:pot_decorations"));
    short.var_i32(2);
    short.var_i32(item_id("minecraft:brick").expect("known item"));
    short.var_i32(item_id("minecraft:flow_pottery_sherd").expect("known item"));
    let payload = set_slot_with_patch("minecraft:decorated_pot", 1, short.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));
    let pot = item.components.pot_decorations.expect("sherds");
    assert!(
        !item.components.has_unmodeled,
        "a short list is legal under list(4) and must not stop the patch"
    );
    assert_eq!(pot.back, None);
    assert_eq!(
        pot.left.as_ref().map(ToString::to_string),
        Some("minecraft:flow_pottery_sherd".to_owned()),
        "the second element is `left`, not the first present one"
    );
    assert_eq!(pot.right, None, "past the declared count");
    assert_eq!(pot.front, None, "past the declared count");
}

/// `minecraft:potion_contents` decodes into the *mixed* colour, and a component
/// placed after it in the same patch proves the reader is still correctly
/// aligned — the general risk with any component whose payload is not
/// length-prefixed.
///
/// Wire shape (`PotionContents.STREAM_CODEC`): `Optional<Holder<Potion>>`,
/// `Optional<Integer>`, `List<MobEffectInstance>`, `Optional<String>`. This
/// stack references `minecraft:swiftness` by holder id with no custom colour and
/// no custom effects, so the expected colour is exactly what
/// `lodestone_data::potion::potion_color` computes from the potion's own
/// built-in effect list — an outside source this test does not itself derive.
#[test]
fn decodes_potion_contents_into_the_mixed_colour_and_stays_aligned() {
    let swiftness = lodestone_data::potion::potion_id("minecraft:swiftness").expect("swiftness");

    let mut patch = Writer::default();
    patch.var_i32(2); // two added components
    patch.var_i32(0); // none removed

    patch.var_i32(component_id("minecraft:potion_contents"));
    patch.bool(true); // potion holder present
    patch.var_i32(swiftness);
    patch.bool(false); // no custom_color
    patch.var_i32(0); // no custom_effects
    patch.bool(false); // no custom_name

    // A second, ordinary component right after it — if `potion_contents`
    // consumed the wrong number of bytes, this decodes garbage or the patch
    // reports unmodeled.
    patch.var_i32(component_id("minecraft:custom_name"));
    write_network_nbt(&mut patch, &Nbt::String("Zoomer".to_owned())).unwrap();

    let payload = set_slot_with_patch("minecraft:potion", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(
        !item.components.has_unmodeled,
        "minecraft:potion_contents is modeled now, so nothing may be flagged partial"
    );
    assert_eq!(
        item.components.potion_color,
        Some(lodestone_data::potion::potion_color(Some(swiftness), None, &[])),
        "the mixed colour must come from the potion's own built-in effect list"
    );
    assert_eq!(
        item.components.custom_name.as_ref().map(Text::to_plain_string),
        Some("Zoomer".to_owned()),
        "the component after potion_contents must still decode — proves alignment"
    );
}

/// A `custom_color` always wins over any effect list — `PotionContents
/// .getColorOr`'s first branch — and a `customEffects` list with one entry
/// carrying a present `hiddenEffect` (the codec's one recursive field) must
/// still leave the reader aligned even though `custom_color` makes the mixed
/// value itself irrelevant to the outcome.
#[test]
fn custom_color_wins_and_a_recursive_hidden_effect_does_not_misalign_the_reader() {
    let speed = 0i32; // minecraft:speed's own network mob-effect id (index 0).

    let mut patch = Writer::default();
    patch.var_i32(2);
    patch.var_i32(0);

    patch.var_i32(component_id("minecraft:potion_contents"));
    patch.bool(false); // no potion holder
    patch.bool(true); // custom_color present
    patch.i32(0x00FF_00FF); // fixed-width int, not a VarInt
    patch.var_i32(1); // one custom effect
    patch.var_i32(speed); // MobEffect holder id
    patch.var_i32(2); // amplifier
    patch.var_i32(600); // duration
    patch.bool(false); // ambient
    patch.bool(true); // showParticles
    patch.bool(true); // showIcon
    patch.bool(true); // hiddenEffect present
    // The nested `Details`, no leading effect id of its own.
    patch.var_i32(0); // amplifier
    patch.var_i32(200); // duration
    patch.bool(false); // ambient
    patch.bool(true); // showParticles
    patch.bool(true); // showIcon
    patch.bool(false); // no further nested hiddenEffect
    patch.bool(false); // no custom_name

    patch.var_i32(component_id("minecraft:custom_name"));
    write_network_nbt(&mut patch, &Nbt::String("Trailing".to_owned())).unwrap();

    let payload = set_slot_with_patch("minecraft:potion", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(!item.components.has_unmodeled);
    assert_eq!(
        item.components.potion_color,
        Some(0xFFFF_00FF),
        "custom_color must win outright and be opaqued"
    );
    assert_eq!(
        item.components.custom_name.as_ref().map(Text::to_plain_string),
        Some("Trailing".to_owned()),
        "the recursive hiddenEffect must not misalign the reader"
    );
}

/// The join-blocking failure this component was modeled for, end to end: an
/// `update_advancements` packet whose icon is a `minecraft:decorated_pot`
/// carrying `minecraft:pot_decorations` decodes, rather than truncating the
/// packet from the icon onward.
///
/// The advancement in question is real — vanilla ships
/// `adventure/craft_decorated_pot_using_only_sherds` with exactly this icon — so
/// any server that has sent an advancement tree hits this. The icon is an
/// `ItemStackTemplate`, whose fields are item-then-count (the reverse of
/// `ItemStack.OPTIONAL_STREAM_CODEC`) and which turns an incomplete patch into a
/// **fatal** decode error rather than a partial stack, so before this component
/// was modeled the whole packet was dropped.
///
/// The control is [`an_advancement_icon_with_an_unmodeled_component_still_fails`]
/// below: byte-identical construction with a genuinely unmodeled component in
/// the icon, which must still fail. Without it, this test would pass against a
/// decoder that had simply stopped raising the error.
#[test]
fn an_advancement_icon_may_be_a_decorated_pot() {
    let payload = advancement_with_icon_patch(&pot_decorations_patch());
    let directives = handle(play::clientbound::UPDATE_ADVANCEMENTS, &payload);
    let ClientEvent::AdvancementsUpdated { added, .. } = expect_single_emit(&directives) else {
        panic!("expected an AdvancementsUpdated emit, got {directives:?}");
    };
    assert_eq!(added.len(), 1);
    let display = added[0]
        .display
        .as_ref()
        .expect("the advancement carries display info");
    assert_eq!(display.icon.item.to_string(), "minecraft:decorated_pot");
    let pot = display
        .icon
        .components
        .pot_decorations
        .as_ref()
        .expect("the icon's sherds");
    assert_eq!(
        pot.front.as_ref().map(ToString::to_string),
        Some("minecraft:snort_pottery_sherd".to_owned()),
        "the icon's fourth sherd, decoded through the template path"
    );
}

/// The control for [`an_advancement_icon_may_be_a_decorated_pot`]: the same
/// packet with an icon carrying a component this build still does not model must
/// still be a fatal decode error, because an `ItemStackTemplate` cannot degrade
/// to a partial stack — everything after it in the packet is unreadable.
///
/// This is what proves the test above is measuring the new component arm rather
/// than a decoder that stopped caring.
#[test]
fn an_advancement_icon_with_an_unmodeled_component_still_fails() {
    let payload = advancement_with_icon_patch(&unmodeled_patch());
    let error = V770Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::UPDATE_ADVANCEMENTS,
            &payload,
        )
        .expect_err("an unmodeled icon component must still be fatal for the packet");
    let text = error.to_string();
    assert!(
        text.contains("unmodeled item component"),
        "expected the advancement-icon cliff, got {text}"
    );
}

/// The `minecraft:pot_decorations` patch bytes shared by the advancement tests:
/// four distinct sherds, `front` last.
fn pot_decorations_patch() -> Vec<u8> {
    let mut patch = Writer::default();
    patch.var_i32(1);
    patch.var_i32(0);
    patch.var_i32(component_id("minecraft:pot_decorations"));
    patch.var_i32(4);
    for sherd in [
        "minecraft:angler_pottery_sherd",
        "minecraft:blade_pottery_sherd",
        "minecraft:howl_pottery_sherd",
        "minecraft:snort_pottery_sherd",
    ] {
        patch.var_i32(item_id(sherd).expect("known sherd item"));
    }
    patch.into_vec()
}

/// Builds an `update_advancements` payload carrying one advancement whose
/// display icon is a `minecraft:decorated_pot` with `patch` as its component
/// patch.
///
/// `DisplayInfo`'s wire order is title, description, icon, frame ordinal, then a
/// **raw big-endian `int`** flag word (`writeInt`, not a byte), then the
/// background identifier only when bit 0 is set, then x and y as floats — see
/// the adapter's own note on `serializeToNetwork` for why that differs from the
/// datapack schema.
fn advancement_with_icon_patch(patch: &[u8]) -> Vec<u8> {
    let mut w = Writer::default();
    w.bool(false); // reset
    w.var_i32(1); // one added advancement
    w.string("minecraft:adventure/craft_decorated_pot_using_only_sherds");
    w.bool(false); // no parent
    w.bool(true); // has display info
    write_network_nbt(&mut w, &Nbt::String("Careful Restoration".to_owned())).unwrap();
    write_network_nbt(&mut w, &Nbt::String("Make a Decorated Pot".to_owned())).unwrap();
    // The icon is an `ItemStackTemplate`: item id first, then count.
    w.var_i32(item_id("minecraft:decorated_pot").expect("known item"));
    w.var_i32(1);
    w.bytes(patch);
    w.var_i32(0); // frame ordinal: task
    w.i32(0); // flag word: no background, no toast, not hidden
    w.f32(1.5); // x
    w.f32(2.5); // y
    w.var_i32(0); // no requirement groups
    w.bool(false); // sends_telemetry_event
    w.var_i32(0); // no removed advancements
    w.var_i32(0); // no progress entries
    w.bool(true); // showAdvancements
    w.into_vec()
}

/// Unwraps a directive batch that must be exactly one [`Directive::Emit`].
fn expect_single_emit(directives: &[Directive]) -> &ClientEvent {
    match directives {
        [Directive::Emit(event)] => event,
        other => panic!("expected a single emit, got {other:?}"),
    }
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
    patch.var_i32(component_id(UNMODELED_COMPONENT));
    write_network_nbt(&mut patch, &Nbt::Compound(Vec::new())).unwrap();

    let payload = set_slot_with_patch("minecraft:diamond_pickaxe", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert_eq!(item.components.damage, Some(42));
    assert!(item.components.has_unmodeled);
}

// ---------------------------------------------------------------------------
// `minecraft:bundle_contents`
// ---------------------------------------------------------------------------

/// Writes one `ItemStackTemplate` (`item_id`, `count`, then an *empty* nested
/// `DataComponentPatch`) — the shape `BundleContents.STREAM_CODEC`'s per-entry
/// codec expects for a contained stack with no components of its own.
fn write_item_stack_template(w: &mut Writer, item: &str, count: i32) {
    w.var_i32(item_id(item).expect("known item"));
    w.var_i32(count);
    w.var_i32(0); // no added components
    w.var_i32(0); // no removed components
}

/// A filled bundle decodes its nested items — including a bundle nested inside
/// it (`BUNDLE_IN_BUNDLE_WEIGHT` is real, so this is not a hypothetical case) —
/// and a component placed *after* `bundle_contents` in the same patch proves
/// the reader is still correctly aligned once the nested list ends, the same
/// alignment risk every unprefixed-payload component in this file carries.
///
/// The two top-level entries and their `item_id`/`count` pairs are pairwise
/// distinct (`torch`/11, `white_bundle`/1) so a transposition of either
/// adjacent VarInt pair cannot survive unnoticed.
#[test]
fn decodes_bundle_contents_including_a_nested_bundle() {
    let mut nested_bundle_patch = Writer::default();
    nested_bundle_patch.var_i32(1); // one added component
    nested_bundle_patch.var_i32(0);
    nested_bundle_patch.var_i32(component_id("minecraft:bundle_contents"));
    nested_bundle_patch.var_i32(1); // one item in the nested bundle
    write_item_stack_template(&mut nested_bundle_patch, "minecraft:iron_ingot", 4);

    let mut patch = Writer::default();
    patch.var_i32(2); // bundle_contents, then damage -- proves alignment survives
    patch.var_i32(0);
    patch.var_i32(component_id("minecraft:bundle_contents"));
    patch.var_i32(2); // two items
    write_item_stack_template(&mut patch, "minecraft:torch", 11);
    // Second entry: a bundle-in-a-bundle -- item id, count, then its own
    // (non-empty) nested patch built above, not `write_item_stack_template`'s
    // empty one.
    patch.var_i32(item_id("minecraft:white_bundle").expect("known item"));
    patch.var_i32(1);
    patch.bytes(nested_bundle_patch.as_slice());
    patch.var_i32(component_id("minecraft:damage"));
    patch.var_i32(9);

    let payload = set_slot_with_patch("minecraft:bundle", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(
        !item.components.has_unmodeled,
        "every component in this patch is modeled"
    );
    assert_eq!(
        item.components.damage,
        Some(9),
        "the reader must still be aligned after the nested list, including the \
         nested bundle inside it"
    );
    assert_eq!(item.components.bundle_contents.len(), 2);
    assert_eq!(item.components.bundle_contents[0].item.to_string(), "minecraft:torch");
    assert_eq!(item.components.bundle_contents[0].count, 11);
    assert_eq!(
        item.components.bundle_contents[1].item.to_string(),
        "minecraft:white_bundle"
    );
    assert_eq!(item.components.bundle_contents[1].count, 1);
    let nested = &item.components.bundle_contents[1].components.bundle_contents;
    assert_eq!(nested.len(), 1, "the bundle-in-a-bundle must decode its own contents");
    assert_eq!(nested[0].item.to_string(), "minecraft:iron_ingot");
    assert_eq!(nested[0].count, 4);
}

/// An unmodeled component inside a *contained* stack is exactly as
/// unrecoverable as one at the top level — `ItemStackTemplate.STREAM_CODEC`'s
/// nested `DataComponentPatch` carries no length prefix either — so it stops
/// the bundle list, flags the outer stack `has_unmodeled`, and (like every
/// other unmodeled-component case in this file) drops the rest of the packet
/// rather than hard-failing the whole connection.
#[test]
fn an_unmodeled_component_inside_a_bundled_item_degrades_gracefully() {
    let mut nested_patch = Writer::default();
    nested_patch.var_i32(item_id("minecraft:torch").expect("known item"));
    nested_patch.var_i32(1);
    nested_patch.var_i32(1); // one added component
    nested_patch.var_i32(0);
    nested_patch.var_i32(component_id(UNMODELED_COMPONENT));
    write_network_nbt(&mut nested_patch, &Nbt::Compound(Vec::new())).unwrap();

    let mut patch = Writer::default();
    patch.var_i32(1);
    patch.var_i32(0);
    patch.var_i32(component_id("minecraft:bundle_contents"));
    patch.var_i32(1); // one item
    patch.bytes(nested_patch.as_slice());

    let payload = set_content(&[
        ("minecraft:bundle", 1, patch.into_vec()),
        ("minecraft:diamond_sword", 1, empty_patch()),
    ]);

    let directives = handle(play::clientbound::CONTAINER_SET_CONTENT, &payload);
    let ClientEvent::ContainerContent { items, .. } = expect_single_emit(&directives) else {
        panic!("wrong event");
    };
    // The unmodeled component inside the bundle ends the whole packet at that
    // slot -- the same "drop the rest, keep what decoded" contract every other
    // unprefixed component in this file gets.
    assert_eq!(
        items.len(),
        1,
        "the slot after the partial bundle must never be read"
    );
    let bundle = items[0].as_ref().expect("the bundle slot");
    assert!(bundle.components.has_unmodeled);
    assert_eq!(
        bundle.components.bundle_contents.len(),
        1,
        "the item decoded before the unmodeled component is retained"
    );
    assert_eq!(
        bundle.components.bundle_contents[0].item.to_string(),
        "minecraft:torch"
    );
}

// ---------------------------------------------------------------------------
// `minecraft:custom_data`, and the multi-item lists that made it fatal
// ---------------------------------------------------------------------------

/// The lobby hotbar. `minecraft:custom_data` is what every Bukkit/Paper plugin
/// stamps on a GUI item, so this is the shape a plugin server actually sends —
/// and while the component was unmodeled it ended the packet at the first slot.
///
/// The value is kept **verbatim** as the network-NBT bytes rather than parsed,
/// and the expected bytes are written out by hand here rather than taken from
/// our own writer: root tag id `0x0a`, then a `TAG_Int` field named `"id"`, then
/// `TAG_End`. The nameless root is the property worth pinning — the derived
/// stream codec is `FriendlyByteBuf.writeNbt`, which writes no root name, so a
/// reader expecting the *named* form would consume the `0x00 0x02` length of the
/// first field's name as a root name and misalign everything after it.
#[test]
fn custom_data_decodes_completely_and_is_kept_opaque() {
    let mut patch = Writer::default();
    patch.var_i32(1);
    patch.var_i32(0);
    patch.var_i32(component_id("minecraft:custom_data"));
    write_network_nbt(
        &mut patch,
        &Nbt::Compound(vec![("id".to_owned(), Nbt::Int(4_919))]),
    )
    .unwrap();

    let payload = set_slot_with_patch("minecraft:compass", 1, patch.as_slice());
    let item = slot_item(&handle(play::clientbound::CONTAINER_SET_SLOT, &payload));

    assert!(
        !item.components.has_unmodeled,
        "minecraft:custom_data is modeled now, so nothing may be flagged partial"
    );
    assert_eq!(
        item.components.custom_data.as_deref(),
        Some(
            &[
                0x0a, // root tag: TAG_Compound, and NO root name follows
                0x03, // field tag: TAG_Int
                0x00, 0x02, b'i', b'd', // field name "id"
                0x00, 0x00, 0x13, 0x37, // 4919
                0x00, // TAG_End closing the compound
            ][..]
        ),
        "the blob is carried byte-for-byte in its network (nameless-root) form"
    );
}

/// Builds a `container_set_content` payload: window 7, state 3, then `patches`
/// one per slot in order, then an empty carried item.
fn set_content(items: &[(&str, i32, Vec<u8>)]) -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(7); // window id
    w.var_i32(3); // state id
    w.var_i32(i32::try_from(items.len()).expect("slot count"));
    for (item, count, patch) in items {
        w.var_i32(*count);
        w.var_i32(item_id(item).expect("known item"));
        w.bytes(patch);
    }
    w.var_i32(0); // carried item: empty
    w.into_vec()
}

fn empty_patch() -> Vec<u8> {
    let mut w = Writer::default();
    w.var_i32(0);
    w.var_i32(0);
    w.into_vec()
}

/// The owner's actual hotbar, as one `container_set_content`: compass, lime dye,
/// diamond sword, every one carrying `minecraft:custom_data`.
///
/// This is the case every existing fixture in this file missed, because they all
/// use a **single**-item packet — the coincidence that let a list caller ignoring
/// the completeness flag survive. The three items are pairwise distinct and their
/// counts are pairwise distinct too (1/5/3), so neither a transposition nor a
/// dropped entry can pass.
#[test]
fn a_three_item_hotbar_all_carrying_custom_data_decodes_whole() {
    let blob = |n: i32| {
        let mut patch = Writer::default();
        patch.var_i32(1);
        patch.var_i32(0);
        patch.var_i32(component_id("minecraft:custom_data"));
        write_network_nbt(&mut patch, &Nbt::Compound(vec![("slot".to_owned(), Nbt::Int(n))]))
            .unwrap();
        patch.into_vec()
    };
    let payload = set_content(&[
        ("minecraft:compass", 1, blob(11)),
        ("minecraft:lime_dye", 5, blob(1)),
        ("minecraft:diamond_sword", 3, blob(4)),
    ]);

    let directives = handle(play::clientbound::CONTAINER_SET_CONTENT, &payload);
    let ClientEvent::ContainerContent { items, .. } = expect_single_emit(&directives) else {
        panic!("wrong event");
    };
    let names: Vec<_> = items
        .iter()
        .map(|slot| {
            let stack = slot.as_ref().expect("every slot is occupied");
            (stack.item.to_string(), stack.count)
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("minecraft:compass".to_owned(), 1),
            ("minecraft:lime_dye".to_owned(), 5),
            ("minecraft:diamond_sword".to_owned(), 3),
        ],
        "all three slots must survive: before custom_data was modeled the list \
         ended at the first one"
    );
}

/// The list-truncation contract, on a **multi-item** packet: an unmodeled
/// component on the *second* of three slots delivers the first two and stops,
/// and the session survives.
///
/// The first slot proves the loop really runs (so an empty result cannot pass as
/// success) and the third proves it really stops.
#[test]
fn a_list_stops_at_the_first_unmodeled_component_and_survives() {
    let payload = set_content(&[
        ("minecraft:compass", 1, empty_patch()),
        ("minecraft:lime_dye", 5, unmodeled_patch()),
        ("minecraft:diamond_sword", 3, empty_patch()),
    ]);

    let directives = handle(play::clientbound::CONTAINER_SET_CONTENT, &payload);
    let ClientEvent::ContainerContent { items, .. } = expect_single_emit(&directives) else {
        panic!("wrong event");
    };
    assert_eq!(
        items.len(),
        2,
        "the partial slot is delivered and the list ends there"
    );
    assert_eq!(items[0].as_ref().expect("slot 0").count, 1);
    let partial = items[1].as_ref().expect("slot 1");
    assert_eq!(partial.item.to_string(), "minecraft:lime_dye");
    assert_eq!(partial.count, 5);
    assert!(partial.components.has_unmodeled);
}

/// `merchant_offers` was the caller that dropped the completeness flag: it read
/// the offer result's stack, discarded the verdict, and went on to read this
/// offer's remaining eight fields — and then the next offer — out of the
/// interior of a component it could not decode.
///
/// The two arms share one fixture generator and differ only in the result stack's
/// patch, which is what makes the second arm's empty verdict attributable to the
/// unmodeled component rather than to a malformed fixture:
///
/// * modeled `custom_data` on all three results → all three offers decode, and
///   the trailing scalars *past* the list are reached, which is only possible if
///   every offer parsed exactly;
/// * an unmodeled component on the second result → the packet is abandoned with
///   no error and no event.
#[test]
fn merchant_offers_abandons_the_packet_instead_of_reading_past_a_partial_result() {
    /// Three offers whose results are pairwise-distinct items with pairwise
    /// distinct counts; `patch_for` supplies each result's component patch.
    fn offers(patch_for: impl Fn(usize) -> Vec<u8>) -> Vec<u8> {
        let results = [
            ("minecraft:compass", 1),
            ("minecraft:lime_dye", 5),
            ("minecraft:diamond_sword", 3),
        ];
        let mut w = Writer::default();
        w.var_i32(9); // window id
        w.var_i32(3); // three offers
        for (index, (item, count)) in results.iter().enumerate() {
            // cost_a: item id, count, empty DataComponentExactPredicate.
            w.var_i32(2 + i32::try_from(index).unwrap());
            w.var_i32(1);
            w.var_i32(0);
            // result
            w.var_i32(*count);
            w.var_i32(item_id(item).expect("known item"));
            w.bytes(&patch_for(index));
            w.bool(false); // no cost_b
            w.bool(false); // out_of_stock
            w.i32(4); // uses         -- five plain writeInts, not VarInts
            w.i32(12); // max_uses
            w.i32(2); // xp
            w.i32(-1); // special_price_diff
            w.f32(0.05); // price_multiplier
            w.i32(7); // demand
        }
        w.var_i32(2); // villager_level -- past the list
        w.var_i32(70); // villager_xp
        w.bool(true); // show_progress
        w.bool(false); // can_restock
        w.into_vec()
    }

    // Arm 1, the premise control: every result carries `custom_data`, which is
    // modeled, so the whole packet — including the scalars behind the list —
    // must decode.
    let modeled = offers(|slot| {
        let mut patch = Writer::default();
        patch.var_i32(1);
        patch.var_i32(0);
        patch.var_i32(component_id("minecraft:custom_data"));
        write_network_nbt(
            &mut patch,
            &Nbt::Compound(vec![("slot".to_owned(), Nbt::Int(i32::try_from(slot).unwrap()))]),
        )
        .unwrap();
        patch.into_vec()
    });
    let modeled_directives = handle(play::clientbound::MERCHANT_OFFERS, &modeled);
    let ClientEvent::MerchantOffersReceived {
        offers: decoded,
        villager_level,
        villager_xp,
        show_progress,
        can_restock,
        ..
    } = expect_single_emit(&modeled_directives)
    else {
        panic!("wrong event");
    };
    assert_eq!(decoded.len(), 3);
    assert_eq!(
        decoded
            .iter()
            .map(|offer| offer.result.as_ref().expect("a result stack").count)
            .collect::<Vec<_>>(),
        vec![1, 5, 3],
        "pairwise-distinct counts, so a transposed or repeated offer cannot pass"
    );
    // Only reachable if all three offers consumed exactly.
    assert_eq!(*villager_level, 2);
    assert_eq!(*villager_xp, 70);
    assert!(*show_progress);
    assert!(!*can_restock);

    // Arm 2: the second result carries a component this build cannot skip. The
    // packet is dropped, cleanly.
    let unmodeled = offers(|slot| {
        if slot == 1 {
            unmodeled_patch()
        } else {
            empty_patch()
        }
    });
    let directives = V770Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::MERCHANT_OFFERS,
            &unmodeled,
        )
        .expect(
            "an unmodeled component must never be a fatal decode: that turns a \
             dropped packet into a dropped session",
        );
    assert!(
        directives.is_empty(),
        "the offer list has no per-entry length prefix and its trailing scalars \
         sit past it, so there is nothing to resynchronise to — the packet is \
         abandoned rather than half-reported: {directives:?}"
    );
}

// ---------------------------------------------------------------------------
// `minecraft:enchantments` — a bare registry id, not a holder-offset one
// ---------------------------------------------------------------------------

/// The bug this section is named for: `minecraft:enchantments`' map key is a
/// *bare* registry id, not the offset-by-one, either-id-or-inline holder shape
/// other component families (`minecraft:instrument`) really do use — see
/// `read_enchantments`'s own doc and `docs/item-data-component-decode.md` for
/// the wire citation. Wire id `0` is therefore an ordinary registry
/// reference, not an "inline holder" marker — the decoder used to reject it
/// outright and fail the whole packet.
///
/// `SET_EQUIPMENT` is the real packet this surfaced on: any entity wearing an
/// item enchanted with whatever occupies registry id 0 lost its entire
/// equipment list. Two slots, pairwise-distinct entity/item/enchantment/level
/// values, and the continuation bit chaining past the id-0 entry into a
/// second, ordinarily-enchanted slot — so this proves both that id 0 no
/// longer aborts the packet and that a non-zero id decodes to *itself*, not
/// to itself minus one.
#[test]
fn enchantment_registry_id_0_is_an_ordinary_reference_not_an_inline_holder() {
    let mut helmet_patch = Writer::default();
    helmet_patch.var_i32(1); // one added component
    helmet_patch.var_i32(0);
    helmet_patch.var_i32(component_id("minecraft:enchantments"));
    helmet_patch.var_i32(1); // one entry
    helmet_patch.var_i32(0); // enchantment registry id 0 -- an ordinary reference
    helmet_patch.var_i32(1); // level I

    let mut sword_patch = Writer::default();
    sword_patch.var_i32(1);
    sword_patch.var_i32(0);
    sword_patch.var_i32(component_id("minecraft:enchantments"));
    sword_patch.var_i32(1);
    sword_patch.var_i32(7); // a pairwise-distinct enchantment id
    sword_patch.var_i32(3); // level III

    let mut w = Writer::default();
    w.var_i32(9); // entity id
    // Slot byte: low 7 bits are the ordinal, the high bit signals another
    // entry follows.
    w.u8(EquipmentSlot::Head.ordinal() | 0x80);
    w.var_i32(1); // stack count
    w.var_i32(item_id("minecraft:diamond_helmet").expect("known item"));
    w.bytes(helmet_patch.as_slice());
    w.u8(EquipmentSlot::MainHand.ordinal()); // last entry, no continuation bit
    w.var_i32(1);
    w.var_i32(item_id("minecraft:diamond_sword").expect("known item"));
    w.bytes(sword_patch.as_slice());

    let directives = handle(play::clientbound::SET_EQUIPMENT, &w.into_vec());
    let ClientEvent::EntityEquipmentUpdated {
        entity_id,
        equipment,
    } = expect_single_emit(&directives)
    else {
        panic!("expected an EntityEquipmentUpdated emit, got {directives:?}");
    };
    assert_eq!(*entity_id, 9);
    assert_eq!(
        equipment.len(),
        2,
        "both slots must survive: an enchantment registry id of 0 must never \
         truncate the equipment list"
    );
    let helmet = equipment[0].item.as_ref().expect("the helmet slot");
    assert!(!helmet.components.has_unmodeled);
    assert_eq!(
        helmet.components.enchantments,
        vec![ItemEnchantment { id: 0, level: 1 }],
        "registry id 0 must decode as an ordinary enchantment reference, not \
         an unsupported inline holder"
    );
    let sword = equipment[1].item.as_ref().expect("the sword slot");
    assert!(!sword.components.has_unmodeled);
    assert_eq!(
        sword.components.enchantments,
        vec![ItemEnchantment { id: 7, level: 3 }],
        "a non-zero id must decode to itself, not to itself minus one"
    );
}
