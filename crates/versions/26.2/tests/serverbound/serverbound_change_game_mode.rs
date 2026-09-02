//! Hermetic byte-exact test for the `change_game_mode` serverbound backlog
//! encoder.
//!
//! Expected payload is built independently of the adapter's own codec, so a
//! symmetric bug cannot pass. Layout is verified against 26.2's
//! `ServerboundChangeGameModePacket` (a single VarInt `GameType` id via
//! `vanilla's own byte buf codecs's own id mapper`: `0` survival, `1` creative, `2` adventure, `3`
//! spectator — matching `GameType`'s declared enum order exactly).
//!
//! Sent by the singleplayer/LAN cheats-enabled F4 game-mode switcher. This
//! action has no live call site yet (no game-mode-switcher UI), but the
//! server remains authoritative regardless — it's free to ignore the
//! request if the sender lacks permission.

use lodestone_model::{ClientAction, ConnectionState, GameMode, VersionAdapter};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;

fn varint(v: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut u = v as u32;
    loop {
        let byte = (u & 0x7F) as u8;
        u >>= 7;
        if u != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
            break;
        }
    }
    out
}

#[test]
fn change_game_mode_ordinals_match_declared_enum_order() {
    let adapter = V770Adapter::new();
    let cases = [
        (GameMode::Survival, 0i32),
        (GameMode::Creative, 1),
        (GameMode::Adventure, 2),
        (GameMode::Spectator, 3),
    ];
    for (mode, ordinal) in cases {
        let encoded = adapter
            .encode_action(
                ConnectionState::Play,
                &ClientAction::ChangeGameMode { mode },
            )
            .expect("encode change game mode");
        assert_eq!(
            encoded,
            Some((play::serverbound::CHANGE_GAME_MODE, varint(ordinal))),
            "mismatch for {mode:?}"
        );
    }
}

#[test]
fn change_game_mode_is_not_encoded_outside_play() {
    let adapter = V770Adapter::new();
    assert_eq!(
        adapter
            .encode_action(
                ConnectionState::Configuration,
                &ClientAction::ChangeGameMode {
                    mode: GameMode::Creative,
                }
            )
            .expect("encode outside play"),
        None
    );
}
