# Protocol 47 hosting

## What it is

`crates/versions/1.8` joins protocol 47 (1.8.8-1.8.9) and also hosts it: the registry resolves protocol 47 to `V47ServerProtocol`; neighbouring protocol numbers stay unhosted.

## How it works

Login goes straight from login success to Play (no configuration exchange). The join packet has a one-byte dimension field; the initial position packet ends at the relative-flags byte, and the client answers it with an ordinary position-and-look packet (no teleport id, so a matching `position_look` stays plain movement).

**Chunks.** Encoding accepts a canonical column only if it covers y=0..=255. It projects non-empty 16-block sections, converts every state through the exact inverse table (numeric block range `0..=197`), writes each legacy state word little-endian in YZX order, then one block-light and one sky-light array per section and the 256-byte biome footer. An unrepresentable state or a short source is an encoding error, never silent air. Block updates and multi-block updates use the same conversion (multi-updates grouped by ascending section, wire order kept within a section). Keep this encoder local to the family: the raw state-word layout differs from the palette format of later families.

**Serverbound lifts.**

| Packet | Becomes |
|---|---|
| `settings` | `ClientInformationChanged` view distance (signed byte; no main-hand field) |
| `position`, `position_look`, `look`, `flying` | the four movement forms |
| `block_dig` statuses 0-2 | break phases (six faces); 3/4 drop stack/one; 5 release item use; no status-6 hand swap on this wire |
| `block_place` | `UseItemOn` (main hand, sequence 0); cursor bytes outside `0..=15` and unknown faces rejected; the in-air sentinel is ignored |
| `chat` | `Chat` (zero timestamp and salt) or, with a leading slash, `ChatCommand` |
| `arm_animation` | `Swing` |
| `entity_action` leave-bed (2) | `PlayerCommand { action: 0 }` (sender id ignored) |
| `use_entity` | interact (0), attack (1), interact-at (2, point read and validated then dropped) |
| `window_click` | `ContainerClicked` (clicked-stack claim left empty) |
| keep-alive | VarInt id (signed), trailing byte rejected |

Replies: `V47ServerProtocol::encode_system_chat` sends a JSON component at position 1; the host broadcasts animations via the legacy `animation` packet.

**Joining adapter details.**
- `block_break_animation` keeps the raw stage byte in `ClientEvent::BlockDestruction` (`0..=9` visible, `-1` clear as `255`).
- `update_time` is two signed `i64` (frozen cycle preserved); `game_state_change` reasons 1/2 end/begin rain, game-mode accepts `0..=3`, other reasons are consumed and ignored.
- `explosion` always carries a `Some` knockback (an all-zero vector included); block offsets are applied to the loaded world as air before the event is emitted.
- Attribute snapshots map the seven known legacy names to canonical keys, give UUID-only modifiers stable private ids, skip unknown names, and reject operation bytes outside `0..=2`.
- Container corrections stay family-level item stacks; the canonical model does not guess pre-flattening damage variants, so the server's correction is the truth.

## How to change it

Extend `V47ServerProtocol` only with packets whose fields have an independent fixture or live-session proof, plus a matching client/server integration assertion when the packet has a visible consumer. Literal-byte controls live in `crates/versions/1.8/tests/` (`server_protocol.rs`, `interaction.rs`, `misc_events.rs`); they exist so field-order and count-width mistakes cannot self-confirm through the codec.

The join replay `crates/versions/1.8/tests/capture_join.rs` replays a committed real 1.8.9 capture (also feeding the 1.7.10 and 1.9.4 captures and requiring neither to join cleanly). Re-record:

```text
./scripts/live-oracles/legacy.sh 1.8.9
cargo test -p lodestone-v1-8 --test capture_join -- --ignored --nocapture record_1_8_9
```

The external-client gate covers protocol 47: `just external-client-acceptance --protocol 47 --output /private/tmp/lodestone-v47` with a driver, recording direct login-to-Play, unbatched chunks, join, movement, one `start_destroy_block` and a clean disconnect. Protocol 47 stays unverified by a real release client until that run passes.

## Configuration

Enable the `v1-8` feature on `lodestone-registry` (no family is on by default).

## Dependencies

`lodestone-server` (hosting trait, canonical chunk source), `lodestone-canonical` (exact legacy inverse mapping), `lodestone-client` with the v1-8 adapter for the in-memory consumer tests.
