# Server driver module layout

## What it is

`crates/lodestone-server/src/server.rs` is the facade of the per-connection server driver: it holds the
shared constants, the public error and summary types and the disconnect reasons, and declares one submodule
per responsibility under `crates/lodestone-server/src/server/`. The driver itself (`serve_connection` and
friends) runs one client connection over any `Transport`: handshake, login, initial chunk stream, then the
play loop.

## How it works

Control flows `entry_points` -> `connection_driver` (handshake, status, login, join) -> `play_loop`
(`serve_play`, the select loop) -> `play_dispatch` (`dispatch_play_packet`, one arm per serverbound
packet) -> the handler modules below.

| Module | Responsibility |
|---|---|
| `entry_points` | the public `serve_connection*` wrappers; each only assembles arguments |
| `connection_driver` | handshake, status ping, login (offline and online), join sequence, hand-off to the play loop |
| `play_loop` | the native and browser `serve_play` loops, vitals ticking, stall watch, client-loaded gate |
| `play_dispatch` | `dispatch_play_packet` |
| `play_state` | teleport acknowledgement tracking and the client movement record |
| `online_mode` | online-mode configuration (session verification, key pair, profile keys) |
| `resource_pack` | resource-pack push feed and recorded responses |
| `join_trace`, `join_snapshots` | opt-in join timing; inventory, experience and attribute snapshots |
| `player_persistence` | native and legacy player save, restore and publish |
| `view_tracker` | which columns a client holds, ring-ordered join batches, view updates |
| `chunk_encoding` | column encoding, light settlement across neighbours, admission footprints |
| `source_ref`, `entity_streaming`, `end_gateway` | borrowed-or-shared chunk source handle; entity visibility trait and streamer; gateway contact rules |
| `block_actions`, `lighting` | digging, breaking, support collapse, tick block updates; light resend and relight batching |
| `use_item_on`, `use_item` | block placement and propagation; held-item use, projectiles, consumption |
| `brewing_stand`, `composter_use` | hand interaction with those two blocks |
| `open_containers`, `container_clicks` | open-window state and screen opening; every container-related click packet |
| `pickups`, `player_effects`, `player_actions` | item and orb pickup; effects on the own player; attack, swing, spectator, recipe book |
| `client_commands`, `health_sync`, `query_tags`, `resident_queries` | small client commands; health and fall publishing; NBT query tags; read-only resident-chunk lookups |

`connection_travel` and `connection_prediction` predate the split and live in `src/` beside `server.rs`
(declared with `#[path]`).

## How to change it

- **Each submodule starts with `use super::*;`** and is glob-imported back by `server.rs`
  (`use self::x::*;`, `pub use` when the module has `pub` items, `pub(crate) use` for `pub(crate)` ones). A
  sibling therefore reaches any item by plain name, and external paths such as
  `crate::server::propagate_placement` keep resolving.
- **Items are `pub(super)`**, which means "visible to the `server` tree". Widen to `pub(crate)` only when
  something outside `server` needs it, and keep the item's `pub use` glob in step.
- **Struct fields are `pub(super)` too**; a field private to its module would not be reachable from the
  handlers that previously shared a file with it.
- **A new seam is a new file**: add `mod name;` plus the matching `use`/`pub use` line in `server.rs` (the
  list is alphabetical) and add a row above. A glob re-export with no item at least that public draws an
  `unused` warning, so pick `use`, `pub(crate) use` or `pub use` by the most public item in the file.
- **Tests live beside their module** as `server/<module>/tests.rs` (`#[cfg(test)] mod tests;` at the end of
  the module). `server/tests.rs` keeps only cross-cutting tests (chunk admission, initial encoding, armour
  snapshots) and the protocol fakes several modules share; helpers a module test needs from there are
  `pub(super)` and imported with `use crate::server::tests::name;`.
- The two largest functions, `serve_play` and `dispatch_play_packet`, are still single functions; split
  them by packet family if they need to change shape, not by moving whole files around.

## Configuration

None of its own. `LODESTONE_JOIN_TRACE=1` enables the join timing in `join_trace`.

## Dependencies

Everything else in `lodestone-server` (`chunk`, `players`, `mobs`, `inventory`, `container_click`,
`block_entities`, `protocol`, ...), plus `lodestone-net` for the transport and `lodestone-model` /
`lodestone-data` for wire and registry types. The module split changed no behaviour and no public path.
