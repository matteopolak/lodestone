# Server game modes

## What it is

The server's game-type state: what mode a connection joins in, how it changes, and the
consequences the server applies (flight permission, instant break, damage immunity). Before
this, `begin_play` hardcoded `game_type: 0` and the `change_game_mode` packet decoded to
`ServerBound::Ignored`, so there was no way to be in creative at all.

## How it works

### The two packets, never one

A mode change is **two** clientbound packets, and splitting them is the defect this was
reported as:

* `ClientboundGameEventPacket` with event code `3` — tells the client *what* mode it is in
  (`ServerProtocol::encode_game_mode`).
* `ClientboundPlayerAbilitiesPacket` — tells the client it *may fly* and *instabuilds*
  (`ServerProtocol::encode_player_abilities`).

Flight permission lives only in the second one. A client told "you are in creative" without
it is in creative and cannot fly. `server.rs`'s `game_mode_directives` returns both as one
array so no call site can send half. Vanilla does the same in `ServerPlayer.setGameMode`.

`Abilities::for_mode` (`protocol.rs`) is `GameType.updatePlayerAbilities` verbatim
(`GameType.java:62-80`). Two things in it are easy to get backwards:

* **Creative does not set `flying`.** It sets `may_fly`. Only spectator ships already
  airborne.
* **`may_build` has no wire bit.** Vanilla's `Abilities.mayBuild` is server-side only; the
  packet carries exactly four flags. It is on the version-free `Abilities` struct because the
  server needs it, and `encode_player_abilities` drops it.

The clientbound flags byte in creative is `0x0D` — `invulnerable | can_fly | instabuild` —
pinned byte-exact by `encode_creative_writes_game_event_3_and_abilities_flags`.

### Where the mode lives, and how it changes

`serve_connection_inner` declares the join mode (**survival** — this crate persists no
per-player game type and reads none from `level.dat`) and hands ownership to `serve_play`,
which threads `&mut GameMode` into `dispatch_play_packet`. Two things move it:

* **`ServerBound::ChangeGameMode`** — the F4 switcher. Answered by echoing the mode actually
  applied, so a client that guessed is corrected rather than trusted.
* **`/gamemode <mode>`** — answered *inside* `dispatch_play_packet`'s `ChatCommand` arm,
  before the host's `CommandDispatch`. Deliberately not routed to the sink: the mode is this
  loop's own state, no host dispatcher can reach it, and every real constructor passes
  `CommandDispatch::none()` (#535), so routing it out would leave creative unreachable in the
  shipping product. Accepts full names, vanilla's `s`/`c`/`a`/`sp` aliases, and `0`–`3`.

**No permission check gates either**, because this crate has no permission model at all — the
same posture the `ChatCommand` arm already documents, and the honest one for a
singleplayer/LAN host.

### Consequences the server applies

| consequence | where | vanilla |
|---|---|---|
| flight | the abilities packet alone | `Abilities.mayfly` |
| instant break | `apply_block_action`'s `StartDestroy` takes the insta-mine exit for every block | `ServerPlayerGameMode.handleBlockBreakAction`'s first branch |
| no drops | `destroy_block`'s `drop_loot: false` — no loot roll, so no RNG draws either | `removeBlock(pos, false)` |
| no fall damage | `fall_status_sample`'s `invulnerable` | `Player.isInvulnerableTo` |
| no border damage | `serve_play`'s `vitals_tick` arm | same |
| no drowning | `vitals.tick(!invulnerable && …)` — the air bar does not deplete either | same |

Only `minecraft:out_of_world` and `minecraft:generic_kill` are in
`#minecraft:bypasses_invulnerability`, and this crate models neither on the player, so
"invulnerable" is total here.

**Infinite blocks needs no code.** Placement resolves the held item but never *spends* it
(`block-edit.md`'s scope note), so every mode already has infinite materials. That is correct
for creative and a known gap for survival.

## How to change it

* **Never send `encode_game_mode` without `encode_player_abilities`.** Go through
  `game_mode_directives`.
* `PlayerVitals` is deliberately mode-free. Gate at the call site, as the three damage sites
  above do, rather than teaching it about game types.
* A new consequence belongs wherever the packet that triggers it is handled, reading
  `*game_mode` (or `Abilities::for_mode(*game_mode)`) there. `dispatch_play_packet` has it as
  `&mut`.

## Known gaps

* **The shell does not produce `ClientAction::ChangeGameMode`** — zero producers outside
  `crates/protocol/`, the outbound-island shape `CLAUDE.md` warns about. So the F4 switcher is
  decoded and honoured but nothing sends it from our own client; `/gamemode` in chat is the
  working switch today.
* **The join mode is not configurable.** `IntegratedServer` has no game-type setting and
  `level.dat`'s `GameType` (which `LevelDatHandle` already reads and writes) is not consulted,
  so every connection joins in survival and must switch.
* **Adventure restrictions are not enforced.** `Abilities::may_build` is computed and sent
  nowhere, and no break/place path consults it, so adventure mode currently differs from
  survival only in the abilities byte.
* **Spectator is a mode name, not a behaviour.** No no-clip, no invisibility, no
  spectator-camera handling.
* **A mode change does not resend the player's inventory or health**, which vanilla does on
  the survival↔creative transition.

## Configuration

None. The join mode is a literal in `serve_connection_inner`.

## Dependencies

* `lodestone_model::GameMode` — the version-free mode enum, shared with the client adapter.
* `crates/protocol/v770/src/server_protocol.rs` for the two encoders and the
  `change_game_mode` decode; `crate::adapter`'s `game_mode_to_ordinal`/`from_ordinal` are the
  single id table both directions use.
