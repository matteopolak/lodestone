# Open to LAN

## What it is

`IntegratedServer::open_to_lan` — the TCP-listening entry point, plus the config surface
for the four subsystems that were implemented, gated, and then unreachable because
`IntegratedServer::bind` took no way to say anything about them: RCON (#331), the
GameSpy4/UT3 query listener (#332), resource-pack pushes (#334), plugin channels (#335) and
commands (#48). Adds vanilla's LAN-discovery broadcast so the world shows up in a client's
multiplayer list without anyone typing an address.

**This is issue #535's scope 2 and 3. Scope 1 — a production caller — is still open**; see
"Known gaps".

## How it works

```rust
let server = IntegratedServer::open_to_lan(
    "0.0.0.0:25565",
    protocol,
    source,
    LanConfig {
        view_radius,
        query: true,
        discovery: Some(LanDiscovery { motd: world_name }),
        commands: dispatch,
        ..LanConfig::default()
    },
).await?;
```

`bind` is now a thin wrapper over it with `LanConfig { view_radius, query: true, ..default }`,
which is `bind`'s pre-#535 behaviour verbatim — no existing caller changes.

### What each field reaches

| field | reaches | was |
|---|---|---|
| `rcon` | `start_rcon` right after the handle is built | only ever called by `tests/rcon.rs` |
| `query` | the UDP listener on the game port's UDP space | always on, unconditionally |
| `discovery` | `spawn_lan_discovery` | did not exist |
| `commands` | every accepted connection's `/`-commands | hardcoded `CommandDispatch::none()` |
| `resource_packs` | `ResourcePackPushFeed` per connection | hardcoded `::default()` |
| `plugin_channels` | `PluginChannelRegistry` per connection | hardcoded `::default()` |

The last three arrive because the accept loop now calls
`serve_connection_with_mob_events_and_commands_shared` (which grew the resource-pack and
plugin-channel parameters and lost its `#[allow(dead_code)]`) instead of
`serve_connection_with_mob_events_shared`, which hardcodes all three.

### Failure policy is deliberately not uniform

* **TCP bind** — fatal. There is no server without it.
* **RCON bind** — fatal, propagated with `?`. A host that asked for a remote console and
  silently did not get one has a security-relevant surprise.
* **Query and discovery binds** — logged and skipped. Neither is needed to play, and taking a
  whole world down for a busy UDP port is the wrong trade.

### LAN discovery

Vanilla's `LanServerPinger` is the entire protocol: one UDP datagram reading
`[MOTD]<name>[/MOTD][AD]<port>[/AD]` to `224.0.2.60:4445`, every 1.5 s, with no handshake and
no reply. `LanDiscovery::payload` builds exactly that string and
`lan_discovery_payload_is_vanillas_literal_format` pins it — an off-by-one in the markers is a
world that never appears in the list, with no error anywhere.

Ping failures log at `debug`, not `warn`: a machine with no route to the multicast group would
otherwise warn every 1.5 s for the life of the session.

### The per-connection feed fix that came with it

`BlockTickFeed`'s doc comment named a one-line LAN gap and said its owner was elsewhere. It is
fixed here: the accept loop builds each `LanSubscriber` with `hub_block_ticks.subscriber()`
rather than `BlockTickFeed::default()`. That keeps the **outbound** queue per-connection (the
relay's drain-all depends on it) while **sharing** the inbound one, so a LAN player placing a
repeater, comparator, observer or redstone torch actually reaches the tick loop that hosts its
scheduled recheck. `default()` shared neither and dropped every such placement silently.

The relay also forwards the effect lane now (with its `except` tag intact), which it did not
before — so a LAN player heard no server-caused sounds at all.

## How to change it

* **Add a config field, not a `bind` parameter.** `bind`'s signature is load-bearing for the
  existing tests and for the "same loop as singleplayer" story.
* A new per-connection surface has to reach the accept loop's `async move`, so clone it out
  *before* the block — the file already has a comment block about exactly this trap, and a
  `.clone()` written inside moves the original in.
* A new background task needs a field, an abort in `Drop`, and a join-or-abort decision in
  `shutdown`. Join it only if it races the `shutdown` notify and returns promptly (like the
  query listener); abort an infinite timer loop, or `shutdown` hangs.

## Known gaps

* **Still zero production callers.** `open_to_lan` is reachable in one call, but nothing in
  `crates/lodestone-shell/` makes it — `net.rs` only ever constructs the in-memory server, and
  `menu/nav.rs` says "There is no LAN discovery here" out loud. That is #535's scope 1 and it
  is a shell change: a menu item that calls `open_to_lan` with the same protocol and source
  singleplayer already uses, and holds the handle for the session.
* **`shutdown().await` on a LAN handle joins the accept loop and the tick loop**, which can
  take a while (it hung indefinitely for a `view_radius: 0` handle while writing the gate
  above). `drop` aborts everything and is what a test should use.
* **The query listener's reported player count comes from the shared `PlayerRegistry`**, so an
  in-memory singleplayer world served alongside is not counted.

## Configuration

`LanConfig` is the whole surface. `LanConfig::default()` is query-on, everything else off,
`view_radius: 0`.

## Dependencies

* `crate::rcon` (`RconConfig`, `spawn_listener`), `crate::query` (`QueryConfig`,
  `QueryServer`), `crate::command::CommandDispatch`,
  `crate::plugin_channels::PluginChannelRegistry`, `crate::server::ResourcePackPushFeed`.
* `tokio::net::{TcpListener, UdpSocket}`; native targets only (the whole module is
  `#[cfg(not(target_arch = "wasm32"))]`).
