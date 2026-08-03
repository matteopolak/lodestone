# Singleplayer

## What it is

Pressing **Singleplayer** on the title screen opens the world list; pressing
**Play Selected World** there starts a real integrated server inside the running
process and connects the client to it over an in-memory duplex. Issue #287.

This is vanilla's architecture, not an approximation of it: there is **one**
client, one version adapter, one event fold and one outbound action queue, and
singleplayer differs from a multiplayer join in exactly one thing — the
`Transport`. Everything downstream of the socket is byte-identical code.

The world is a fixed seed handed to the bundled overworld generator. There is
**no save format, no save directory and no world creation** (world creation is
#190). Nothing is written to disk.

## How it works

### The chain, end to end

| step | where |
|---|---|
| Play Selected World pressed | `menu/world_select.rs` → `WorldSelectOutcome::Play` |
| lifted to an app action | `menu/nav.rs`'s `apply_world_select` → `MenuAction::Singleplayer` |
| the app's one side effect | `app.rs`'s `apply_menu_action` → `begin_singleplayer` |
| resolve the *serverbound* version half | `app.rs`'s `launch_singleplayer` → `lodestone_registry::server_protocol_for_protocol(protocol)` |
| start the server, connect the client | `net.rs`'s `NetClient::open_singleplayer` → `Origin::Integrated` |
| serve | `lodestone_server::IntegratedServer::open_in_memory(protocol, source, view_radius)` |
| join | `lodestone_client::ClientBuilder::connect_with(duplex)` |
| render | `app.rs`'s `install_session_render_sources`, shared with `connect_to` |

`MenuAction::Singleplayer` existed with **no producer at all** between #397 and
#287 — it was kept as exactly this seam. That is worth knowing because "the
variant exists and is matched exhaustively" was true the whole time, which is
what an island looks like from the inside.

### The version seam runs in both directions

`cargo check -p lodestone-shell --no-default-features` is a required health check
whose entire job is proving the shell compiles with **no version family**. So the
shell cannot name `V770ServerProtocol`, and the previous `launch_singleplayer`
was a deliberate stub that refused to.

The fix is the mirror of what the clientbound side already did:

```rust
// clientbound (net.rs, since forever)
let adapter = lodestone_registry::adapter_for_protocol(protocol)?;      // Box<dyn VersionAdapter>
// serverbound (app.rs, #287)
let server = lodestone_registry::server_protocol_for_protocol(protocol)?; // Box<dyn ServerProtocol>
```

`lodestone-registry` is the one crate allowed to name version crates, and it does
so only through optional feature-gated dependencies. `SERVER_FAMILIES` is a
second table beside `FAMILIES`, gated identically, so deleting a family's folder
still removes one line from each.

Three decisions in there that are not obvious:

- **`SERVER_FAMILIES` is a separate table, not a field on `Family`.** The two sets
  are genuinely different: a family can have a `VersionAdapter` (joinable) and no
  `ServerProtocol` (unhostable). A fused table would need an `Option` that is
  `None` for three of four entries, which reads as an oversight rather than a
  fact. `supports` delegates to the family's own `VersionAdapter::supports`, so
  the two directions cannot drift about which protocol numbers a family covers.
- **`lodestone-registry`'s dependency on `lodestone-server` is required, not
  optional.** Feature-gating it looks tidier and is wrong: the shell calls
  `server_protocol_for_protocol` unconditionally, so a `#[cfg(feature = …)]`
  function would *stop existing* in a version-free build — turning the
  `--no-default-features` check into a compile failure instead of the `None` the
  shell is supposed to observe and report. The edge is version-free →
  version-free, so `cargo run -p xtask -- check-isolation` has nothing to say
  about it.
- **`Box<dyn ServerProtocol>` needed an impl to be servable.** The trait was
  already object-safe, but `IntegratedServer::open_in_memory` takes
  `P: ServerProtocol` *by value*, and `Box<dyn ServerProtocol>` does not implement
  the trait for free. `lodestone-server`'s `impl<P: ServerProtocol + ?Sized>
  ServerProtocol for Box<P>` forwards all eighteen methods.

### The server lives on the net thread

`NetClient::open_singleplayer` does not spawn anything new. The existing net
thread already owns a current-thread tokio runtime; `IntegratedServer::open_in_memory`
spawns its serving task onto whatever runtime is entered, so hosting it there puts
the server tick, the client driver and the event fold on **one thread with no
cross-thread synchronisation at all**, and makes the server's lifetime exactly the
session's.

The render loop is unaffected: it still drains `NetUpdate`s once per frame, exactly
as for a multiplayer session.

### What the player waits for

Terrain is generated lazily per column, but the *initial view* is generated before
the client finishes loading. At roughly **12 ms per column** (measured — see
[`chunk-memory-pool-footprint.md`](./chunk-memory-pool-footprint.md)) the default
`render_distance` of 8 is 289 columns, so expect a few seconds on the loading
screen. That is generation cost, not a stall: the shell keeps rendering and the
world appears as batches arrive.

`view_radius` is `Config::render_distance`, the same number the camera far plane
and the mesher use, so the server never sends a column the renderer would discard
and never withholds one it wants.

## How to change it

- **The world's identity lives in `menu/world_select.rs`'s `BUNDLED_WORLD`** —
  label and seed together, because those are the same fact. `app.rs` reads the seed
  from there. If a real world list ever lands, `WorldSelectOutcome::Play` becomes
  `Play(WorldEntry)` and both ends change at once, by construction.
- **The seed is fixed deliberately.** A random seed per launch would make "the
  world" a different world every time it is opened, which is *worse* than not
  persisting it — a player would read the changing surroundings as a bug. A fixed
  seed plus a deterministic generator is the closest thing to persistence there is
  without a save format, and the row label says the world is generated.
- **The row label has a 44-character ceiling.** Vanilla's `NoWorldsEntry` gives its
  `StringWidget` no `maxWidth`, so nothing clips it and a longer string overhangs
  the 266 px row. `the_world_list_row_label_fits_the_row_it_is_centred_in`
  measures it.
- **Adding a `ServerProtocol` method?** Add its forward to the `Box<P>` impl in
  `lodestone-server`'s `protocol.rs`. Thirteen of the eighteen methods have
  defaults, so a missing forward is **not a compile error** — the box silently
  answers with the trait default, and a boxed v770 would stop sending (say)
  keep-alives while a directly-owned one kept working. That asymmetry only shows
  up in singleplayer, which is the path with no live oracle.
  `a_boxed_protocol_answers_exactly_as_the_concrete_one_does` compares all
  eighteen against a spy, with a control proving the spy's answers differ from the
  defaults.
- **Hosting a second version family** is one `SERVER_FAMILIES` entry, once that
  family has a `ServerProtocol` impl. Nothing else changes; the shell already asks
  by number.
- **A `LaunchError` is reported, never routed around.** There is exactly one
  variant and it is a build property: `NoVersionFamily`. Everything else on the
  path is infallible — `open_in_memory` binds no port and `connect_with` dials
  nothing — so a successful `launch_singleplayer` means a server is running.
  Login is still asynchronous, so *joined* is proven by reaching
  `Screen::Playing`, not by the `Ok`.

## Proof, and what each piece can and cannot see

| gate | crate | what it would catch |
|---|---|---|
| `pressing_play_reaches_a_running_integrated_server` | `lodestone-shell` (`app.rs`) | the whole shell chain: registry lookup → net thread → duplex → serving loop → real v770 wire → client decode, ending at a `NetUpdate` the frame loop consumes |
| `play_selected_world_asks_the_app_to_start_singleplayer` | `lodestone-shell` (`menu/nav.rs`) | the button producing `MenuAction::Singleplayer` at all, by click *and* by keyboard |
| `a_registry_resolved_server_protocol_serves_a_real_joined_session` | `protocol/v770` | the production path with no shell: a protocol *number* through the real registry to a joined session with terrain |
| `a_boxed_protocol_answers_exactly_as_the_concrete_one_does` | `lodestone-server` | a `ServerProtocol` method that is not forwarded through `Box` |
| `default_build_has_no_families` | `lodestone-registry` | `server_protocol_for_protocol` answering something in a version-free build |

Two things about that list are deliberate.

**Chunks, not login, is the load-bearing assertion** in the two end-to-end gates.
Login is five `ServerProtocol` methods with no trait defaults, so it cannot
silently fall through the box; terrain is where a half-wired server shows up, and
it is the only thing that proves a *world* exists rather than a handshake. A
session that logs in and streams nothing is exactly the shape of the chunk
blackouts `CLAUDE.md` records.

**`server_liveness.rs` was not enough**, even though it already joined the real
`V770ServerProtocol` over an in-memory duplex before #287. It names
`V770ServerProtocol` directly, which is the one thing the shell may not do — so it
proved the server works and said nothing about the path production takes. That is
why `singleplayer_seam.rs` exists beside it.

## Configuration

| knob | effect |
|---|---|
| `--render-distance <n>` | the server's `view_radius`, hence the initial view's size and the load time |
| `--protocol <n>` | which family the registry is asked for, on both sides |
| `lodestone-shell`'s `live` feature (default **on**) | compiles `v770` into the registry; without it `server_protocol_for_protocol` returns `None` and Singleplayer reports `NoVersionFamily` |
| `BUNDLED_WORLD.seed` | the world |

## Dependencies

- `lodestone-server` — `IntegratedServer`, `ServerProtocol`, `overworld_chunk_source`
  (the bundled 26.2 shape+surface generator).
- `lodestone-registry` — the protocol-number → adapter / server-protocol lookup.
- `lodestone-client` — `ClientBuilder::connect_with`, the `Transport` seam.
- `crates/protocol/v770` — `V770ServerProtocol`, named **only** by the registry.

## Related

- [Served session liveness](./served-session-liveness.md) — keep-alive, the
  day/night clock and view streaming, i.e. what makes the served session survive
  once it exists.
- [World select, with creation disabled](./world-select.md) — the screen the launch
  button lives on.
- [Block edit](./block-edit.md) — digging and placing against the integrated
  server.
