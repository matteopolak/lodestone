//! Hermetic decode gates for protocol 776 `map_item_data` (id 51) and
//! `update_advancements` (id 130).
//!
//! Byte vectors are hand-built from the 26.2 decompiled record definitions
//! (`ClientboundMapItemDataPacket`, `MapItemSavedData.MapPatch`,
//! `ClientboundUpdateAdvancementsPacket`, `Advancement`, `DisplayInfo`), never
//! round-tripped through anything of ours — this crate has no encoder for either
//! packet, so there is nothing symmetric available to be wrong in both
//! directions.
//!
//! The three field orders these gates pin down, each of which is wrong in the
//! obvious reading:
//!
//! * `MapPatch` writes **width, height, startX, startY** — not its declaration
//!   order — and spells "absent" as a zero *width* byte with no boolean tag.
//! * `DisplayInfo`'s flag word is a raw big-endian `int`, and `announceChat` is
//!   not on the wire, so the bits are `1 = background`, `2 = showToast`,
//!   `4 = hidden`.
//! * `AdvancementType`'s ordinals are `TASK, CHALLENGE, GOAL`.

use lodestone_model::{
    AdvancementFrame, ClientEvent, ConnectionState, Directive, VersionAdapter,
};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// Independent VarInt encoder (not the codec under test).
fn var_i32(value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    #[allow(clippy::cast_sign_loss)]
    let mut v = value as u32;
    loop {
        #[allow(clippy::cast_possible_truncation)]
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}

fn utf(value: &str) -> Vec<u8> {
    let mut out = var_i32(i32::try_from(value.len()).unwrap());
    out.extend_from_slice(value.as_bytes());
    out
}

/// A network-NBT string component, which is how `TRUSTED_STREAM_CODEC` writes a
/// `Component.literal`: TAG_String (0x08) with no name, then a big-endian u16
/// length and the bytes.
fn nbt_string(value: &str) -> Vec<u8> {
    let mut out = vec![0x08];
    out.extend_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
    out.extend_from_slice(value.as_bytes());
    out
}

fn handle(packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle packet")
}

fn expect_err(packet_id: i32, payload: &[u8]) {
    assert!(
        V770Adapter::new()
            .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
            .is_err(),
        "expected packet {packet_id} to be rejected"
    );
}

/// A map update carrying both a decoration and a 2×3 sub-rectangle patch at an
/// offset — the shape vanilla actually sends while a player walks.
#[test]
fn map_item_data_decodes_decorations_and_a_sub_rectangle_patch() {
    let mut payload = var_i32(9); // MapId
    payload.push(3u8); // scale
    payload.push(1u8); // locked
    payload.push(1u8); // decorations present
    payload.extend_from_slice(&var_i32(1)); // one decoration
    payload.extend_from_slice(&var_i32(24)); // map_decoration_type id 24 = banner_red
    #[allow(clippy::cast_sign_loss)]
    payload.push(-7i8 as u8); // x
    payload.push(11u8); // y
    payload.push(0x13u8); // rot 19, which vanilla's own record masks to 3
    payload.push(1u8); // name present
    payload.extend_from_slice(&nbt_string("Home"));
    // MapPatch: width, height, startX, startY, then the byte array.
    payload.push(2u8);
    payload.push(3u8);
    payload.push(40u8);
    payload.push(50u8);
    payload.extend_from_slice(&var_i32(6));
    payload.extend_from_slice(&[10, 11, 12, 13, 14, 15]);

    let directives = handle(play::clientbound::MAP_ITEM_DATA, &payload);
    let [Directive::Emit(ClientEvent::MapItemData {
        map_id,
        scale,
        locked,
        decorations,
        color_patch,
    })] = directives.as_slice()
    else {
        panic!("expected one MapItemData emit, got {directives:?}");
    };
    assert_eq!(*map_id, 9);
    assert_eq!(*scale, 3);
    assert!(*locked);

    let decorations = decorations.as_ref().expect("decorations present");
    assert_eq!(decorations.len(), 1);
    assert_eq!(decorations[0].kind.to_string(), "minecraft:banner_red");
    assert_eq!(decorations[0].x, -7);
    assert_eq!(decorations[0].y, 11);
    assert_eq!(decorations[0].rotation, 3, "rot is masked with & 15");
    assert_eq!(
        decorations[0].name.as_ref().map(lodestone_model::Text::to_plain_string),
        Some("Home".to_string())
    );

    let patch = color_patch.as_ref().expect("patch present");
    assert_eq!(
        (patch.start_x, patch.start_y, patch.width, patch.height),
        (40, 50, 2, 3),
        "width/height precede startX/startY on the wire"
    );
    assert_eq!(patch.colors, vec![10, 11, 12, 13, 14, 15]);
}

/// A zero width byte is the absent patch, with no length prefix and no boolean
/// tag after it. Reading a `bool` here instead would consume the width and then
/// read the rest of the packet one byte out of phase.
#[test]
fn a_zero_width_byte_is_an_absent_patch() {
    let mut payload = var_i32(1);
    payload.push(0u8); // scale
    payload.push(0u8); // unlocked
    payload.push(0u8); // no decorations
    payload.push(0u8); // width 0 -> Optional.empty, and the packet ends here

    let directives = handle(play::clientbound::MAP_ITEM_DATA, &payload);
    let [Directive::Emit(ClientEvent::MapItemData {
        decorations,
        color_patch,
        ..
    })] = directives.as_slice()
    else {
        panic!("expected one MapItemData emit, got {directives:?}");
    };
    assert!(decorations.is_none(), "absent is not empty");
    assert!(color_patch.is_none());
}

/// The colour array length must equal `width * height`; a mismatch means the
/// patch geometry was misread and the pixels would be blitted into the wrong
/// rows.
#[test]
fn a_patch_whose_colour_array_disagrees_with_its_geometry_is_refused() {
    let mut payload = var_i32(1);
    payload.push(0u8);
    payload.push(0u8);
    payload.push(0u8);
    payload.push(4u8); // width
    payload.push(4u8); // height -> 16 expected
    payload.push(0u8);
    payload.push(0u8);
    payload.extend_from_slice(&var_i32(3));
    payload.extend_from_slice(&[1, 2, 3]);
    expect_err(play::clientbound::MAP_ITEM_DATA, &payload);
}

/// One advancement with full display info plus a progress entry, exercising the
/// flag word, the frame ordinal, the optional background, and the nullable
/// `Instant` inside `CriterionProgress`.
#[test]
fn update_advancements_decodes_display_flags_frame_and_progress() {
    let mut payload = vec![1u8]; // reset
    payload.extend_from_slice(&var_i32(1)); // one AdvancementHolder
    payload.extend_from_slice(&utf("minecraft:story/root"));
    payload.push(0u8); // no parent
    payload.push(1u8); // display present
    payload.extend_from_slice(&nbt_string("Minecraft"));
    payload.extend_from_slice(&nbt_string("The heart and story of the game"));
    // ItemStackTemplate: item holder id, count, then an empty component patch.
    // Item id 1 rather than 0: 0 is `minecraft:air`, which the template's own
    // constructor rejects.
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(0)); // components added
    payload.extend_from_slice(&var_i32(0)); // components removed
    payload.extend_from_slice(&var_i32(1)); // AdvancementType ordinal 1 = CHALLENGE
    payload.extend_from_slice(&3i32.to_be_bytes()); // flags: 1 background | 2 showToast
    payload.extend_from_slice(&utf("minecraft:textures/gui/advancements/backgrounds/stone.png"));
    payload.extend_from_slice(&0.5f32.to_be_bytes()); // x
    payload.extend_from_slice(&2.25f32.to_be_bytes()); // y
    // AdvancementRequirements: one group of two names (an anyOf).
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(2));
    payload.extend_from_slice(&utf("crafting_table"));
    payload.extend_from_slice(&utf("stone"));
    payload.push(1u8); // sendsTelemetryEvent

    payload.extend_from_slice(&var_i32(1)); // one removed id
    payload.extend_from_slice(&utf("minecraft:recipes/misc/gone"));

    payload.extend_from_slice(&var_i32(1)); // one progress entry
    payload.extend_from_slice(&utf("minecraft:story/root"));
    payload.extend_from_slice(&var_i32(2)); // two criteria
    payload.extend_from_slice(&utf("crafting_table"));
    payload.push(1u8); // obtained
    payload.extend_from_slice(&1_700_000_000_123i64.to_be_bytes());
    payload.extend_from_slice(&utf("stone"));
    payload.push(0u8); // not obtained

    payload.push(1u8); // showAdvancements

    let directives = handle(play::clientbound::UPDATE_ADVANCEMENTS, &payload);
    let [Directive::Emit(ClientEvent::AdvancementsUpdated {
        reset,
        added,
        removed,
        progress,
        show_advancements,
    })] = directives.as_slice()
    else {
        panic!("expected one AdvancementsUpdated emit, got {directives:?}");
    };
    assert!(*reset);
    assert!(*show_advancements);
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].to_string(), "minecraft:recipes/misc/gone");

    assert_eq!(added.len(), 1);
    let entry = &added[0];
    assert_eq!(entry.id.to_string(), "minecraft:story/root");
    assert!(entry.parent.is_none());
    assert!(entry.sends_telemetry_event);
    assert_eq!(entry.requirements, vec![vec!["crafting_table".to_string(), "stone".to_string()]]);

    let display = entry.display.as_ref().expect("display present");
    assert_eq!(display.frame, AdvancementFrame::Challenge, "ordinal 1 is CHALLENGE, not GOAL");
    assert_eq!(display.title.to_plain_string(), "Minecraft");
    assert!(display.show_toast, "flag bit 2");
    assert!(!display.hidden, "flag bit 4 was clear");
    assert!(display.background.is_some(), "flag bit 1 gates the background id");
    assert_eq!(display.icon.count, 1);
    // The layout the server computed, which is the only place it exists.
    assert!((display.x - 0.5).abs() < f32::EPSILON);
    assert!((display.y - 2.25).abs() < f32::EPSILON);

    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].0.to_string(), "minecraft:story/root");
    assert_eq!(
        progress[0].1,
        vec![
            ("crafting_table".to_string(), Some(1_700_000_000_123)),
            ("stone".to_string(), None),
        ]
    );
}

/// A display-less advancement (a recipe unlock, the ~1560-entry majority of the
/// real tree) is the minimal holder: no parent flag set, no display, one
/// requirement group.
#[test]
fn update_advancements_decodes_a_display_less_holder() {
    let mut payload = vec![0u8]; // not a reset
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&utf("minecraft:recipes/building_blocks/oak_planks"));
    payload.push(1u8); // parent present
    payload.extend_from_slice(&utf("minecraft:recipes/root"));
    payload.push(0u8); // no display
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&var_i32(1));
    payload.extend_from_slice(&utf("has_the_recipe"));
    payload.push(0u8); // sendsTelemetryEvent
    payload.extend_from_slice(&var_i32(0)); // no removals
    payload.extend_from_slice(&var_i32(0)); // no progress
    payload.push(1u8);

    let directives = handle(play::clientbound::UPDATE_ADVANCEMENTS, &payload);
    let [Directive::Emit(ClientEvent::AdvancementsUpdated { added, .. })] = directives.as_slice()
    else {
        panic!("expected one AdvancementsUpdated emit, got {directives:?}");
    };
    assert_eq!(added.len(), 1);
    assert!(added[0].display.is_none());
    assert_eq!(
        added[0].parent.as_ref().map(ToString::to_string),
        Some("minecraft:recipes/root".to_string())
    );
}

/// Trailing bytes are a decode error, not a shrug: it is the symptom of every
/// field-order mistake above.
#[test]
fn trailing_bytes_after_an_advancement_packet_are_refused() {
    let mut payload = vec![0u8];
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.extend_from_slice(&var_i32(0));
    payload.push(1u8);
    payload.push(0xFF); // one byte too many
    expect_err(play::clientbound::UPDATE_ADVANCEMENTS, &payload);
}

/// The two encoders that had no v770 override at all, so every advancement and
/// statistic the server tracked reached the wire as `ServerDirective::None`.
mod server_encoders {
    use lodestone_server::{
        Advancement, AdvancementProgressUpdate, AdvancementUpdate, ServerDirective, ServerProtocol,
        StatKey, StatType,
    };
    use lodestone_v770::V770ServerProtocol;
    use lodestone_v770::packet_ids::play;

    use super::var_i32;

    /// `update_advancements` produces a real frame with the exact body vanilla's
    /// own reader expects, byte for byte.
    #[test]
    fn update_advancements_encodes_a_real_frame() {
        let update = AdvancementUpdate {
            reset: true,
            added: vec![
                Advancement::new("minecraft:story/root", vec![vec!["got_it".to_string()]], false),
            ],
            removed: vec!["minecraft:story/gone".to_string()],
            progress: vec![AdvancementProgressUpdate {
                id: "minecraft:story/root".to_string(),
                criteria: vec![("got_it".to_string(), Some(1_700_000_000_123))],
            }],
            show_advancements: true,
        };

        let mut expected = vec![1u8]; // reset
        expected.extend_from_slice(&var_i32(1)); // one added
        expected.extend_from_slice(&var_i32(20));
        expected.extend_from_slice(b"minecraft:story/root");
        expected.push(0u8); // no parent
        expected.push(0u8); // no display
        expected.extend_from_slice(&var_i32(1)); // one requirement group
        expected.extend_from_slice(&var_i32(1)); // one criterion in it
        expected.extend_from_slice(&var_i32(6));
        expected.extend_from_slice(b"got_it");
        expected.push(0u8); // sendsTelemetryEvent
        expected.extend_from_slice(&var_i32(1)); // one removed
        expected.extend_from_slice(&var_i32(20));
        expected.extend_from_slice(b"minecraft:story/gone");
        expected.extend_from_slice(&var_i32(1)); // one progress entry
        expected.extend_from_slice(&var_i32(20));
        expected.extend_from_slice(b"minecraft:story/root");
        expected.extend_from_slice(&var_i32(1)); // one criterion
        expected.extend_from_slice(&var_i32(6));
        expected.extend_from_slice(b"got_it");
        expected.push(1u8); // obtained
        expected.extend_from_slice(&1_700_000_000_123i64.to_be_bytes());
        expected.push(1u8); // showAdvancements

        assert_eq!(
            V770ServerProtocol.encode_update_advancements(&update),
            ServerDirective::Send {
                packet_id: play::clientbound::UPDATE_ADVANCEMENTS,
                payload: expected,
            }
        );
    }

    /// `award_stats` resolves each key in the registry its stat type dispatches
    /// on — `mined` in the *block* registry, `killed` in `entity_type` — and
    /// skips a key that resolves in neither rather than inventing an id.
    #[test]
    fn award_stats_encodes_registry_ids_per_stat_type() {
        let stats = vec![
            (StatKey::new(StatType::Mined, "minecraft:stone"), 12),
            (StatKey::new(StatType::Custom, "play_time"), 4200),
            (StatKey::new(StatType::Killed, "minecraft:not_a_mob"), 3),
        ];
        let ServerDirective::Send { packet_id, payload } =
            V770ServerProtocol.encode_award_stats(&stats)
        else {
            panic!("award_stats must send");
        };
        assert_eq!(packet_id, play::clientbound::AWARD_STATS);

        let mut expected = var_i32(2); // the unresolvable entity key is skipped
        expected.extend_from_slice(&var_i32(0)); // stat_type mined
        expected.extend_from_slice(&var_i32(1)); // block registry id of stone
        expected.extend_from_slice(&var_i32(12));
        expected.extend_from_slice(&var_i32(8)); // stat_type custom
        expected.extend_from_slice(&var_i32(1)); // custom_stat play_time
        expected.extend_from_slice(&var_i32(4200));
        assert_eq!(payload, expected);
    }
}
