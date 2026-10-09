# Server driver module layout

## What it is

`crates/lodestone-server/src/server.rs` is the facade of the per-connection driver: shared constants, public error and summary types, disconnect reasons, and one submodule per responsibility under `src/server/`. `serve_connection` and friends run one client over any `Transport`: handshake, login, initial chunk stream, then the play loop.

## How it works

Control flows `entry_points` -> `connection_driver` (handshake, status, login, join) -> `play_loop` (`serve_play`) -> `play_dispatch` (`dispatch_play_packet`, one arm per serverbound packet) -> handler modules.

| Module | Responsibility |
|---|---|
| `entry_points` | public `serve_connection*` wrappers (assemble arguments only) |
| `connection_driver` | handshake, status ping, login (offline and online), join sequence, hand-off |
| `play_loop` | native and browser `serve_play` loops, vitals ticking, stall watch, client-loaded gate |
| `play_dispatch`, `play_state` | `dispatch_play_packet`; teleport acknowledgements and client movement record |
| `online_mode`, `resource_pack` | online configuration; resource-pack push feed and responses |
| `join_trace`, `join_snapshots` | opt-in join timing; inventory, experience, attribute snapshots |
| `player_persistence` | native and legacy player save, restore, publish |
| `view_tracker`, `chunk_encoding` | held columns, ring-ordered join batches, view updates; column encoding, light settlement, admission footprints |
| `source_ref`, `entity_streaming`, `end_gateway` | borrowed-or-shared chunk source; entity visibility trait and streamer; gateway contact |
| `block_actions`, `lighting` | digging, breaking, support collapse, tick updates; light resend and relight batching |
| `use_item_on`, `use_item`, `brewing_stand`, `composter_use` | placement and propagation; held-item use, projectiles, consumption; those two blocks |
| `open_containers`, `container_clicks` | window state and opening; every container click |
| `pickups`, `player_effects`, `player_actions` | item and orb pickup; own-player effects; attack, swing, spectator, recipe book |
| `client_commands`, `health_sync`, `query_tags`, `resident_queries` | small commands; health and fall publishing; NBT query tags; read-only resident lookups |

`connection_travel` and `connection_prediction` predate the split and sit in `src/` beside `server.rs` via `#[path]`.

## How to change it

- Each submodule starts with `use super::*;` and `server.rs` glob-imports it back (`use self::x::*;`; `pub use` if it has `pub` items, `pub(crate) use` for `pub(crate)`), so siblings reach items by plain name and paths like `crate::server::propagate_placement` keep resolving.
- Items and struct fields are `pub(super)` (visible to the `server` tree); widen to `pub(crate)` only for outside users, keeping the glob in step. A glob re-export wider than its most public item draws an `unused` warning.
- A new seam is a new file: `mod name;` plus its `use` line in `server.rs` (alphabetical) and a table row.
- Tests sit beside modules as `server/<module>/tests.rs`; `server/tests.rs` keeps cross-cutting tests (admission, initial encoding, armour snapshots) and shared protocol fakes (`pub(super)`, imported via `use crate::server::tests::name;`).
- `serve_play` and `dispatch_play_packet` are still single large functions; split them by packet family rather than moving files.

## Configuration

`LODESTONE_JOIN_TRACE=1` enables `join_trace` timing.

## Dependencies

The rest of `lodestone-server` (`chunk`, `players`, `mobs`, `inventory`, `container_click`, `block_entities`, `protocol`), `lodestone-net` for the transport, `lodestone-model` and `lodestone-data` for wire and registry types.
