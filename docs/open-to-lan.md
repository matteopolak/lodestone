# Open to LAN

## What it is

`IntegratedServer::open_to_lan` — the TCP-listening entry point, plus the config surface
for the four subsystems that were implemented, gated, and then unreachable because
`IntegratedServer::bind` took no way to say anything about them: RCON (#331), the
GameSpy4/UT3 query listener (#332), resource-pack pushes (#334), plugin channels (#335) and
commands (#48). Adds vanilla's LAN-discovery broadcast so the world shows up in a client's
multiplayer list without anyone typing an address.

Scopes 1, 2 and 3 of issue #535 are all closed: the pause menu's **Open to LAN** button
(vanilla's `menu.multiplayerOptions.button`, whose `en_us` value really is "Open to LAN") is
the production caller.

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

## The shell caller

`PauseButton::OpenToLan` → `MenuAction::OpenToLan` → `WindowApp::open_current_world_to_lan`
(`app/session.rs`) → `NetClient::open_to_lan` → `net.rs`'s `open_lan_world`.

### It reopens the world instead of publishing the live handle

`open_to_lan` **constructs** a server — it binds the listener, builds the `ChunkStore` and
spawns the tick loop. There is no `publish()` on a running `IntegratedServer`, so the only
way to get a socket in front of the world you are in is to end the current session and open
the same launch again with LAN on. `WindowApp::hosted_world` remembers the launch for exactly
that; the player sees the loading screen briefly and then rejoins over `127.0.0.1`.

`Sim::end_session` runs **first**, and the order is load-bearing: the running server holds the
world's region directory, and binding a second one over the same files is two writers to the
same chunks.

### One transport, no privileged host

The host's own client dials the socket like everybody else. Nothing on this path is a second,
special kind of connection — which is what stops "works in singleplayer, broken over LAN"
from being possible for anything the host does.

### The autosave is shell-driven, and one half of persistence is still missing

`open_to_lan` sets `save: None`, so it starts no autosave task and flushes nothing at
shutdown. `net.rs`'s LAN branch therefore wraps the generator in a `RegionChunkSource` itself
(so a saved world's terrain and edits **load**), holds the `WorldSaveHandle`, writes on the
same `AUTOSAVE_INTERVAL` singleplayer uses, and writes once more before dropping the handle.

**What still does not persist: block entities and scheduled ticks placed while hosting.**
`open_to_lan` builds its own `BlockEntityHandle::default()` rather than taking the source's,
so chest contents animate and furnaces cook but none of it reaches
`WorldSaveHandle::extras_for`. Closing that is a `crates/lodestone-server` change — `LanConfig`
growing a world directory and reusing `open_persistent_with_mobs`' save wiring — not a shell
one.

### Teardown uses `drop`, not `shutdown().await`

`shutdown().await` joins the accept loop, which parks in `accept()` where the shutdown notify
cannot reach it; it hung indefinitely for a `view_radius: 0` handle while #535's own gate was
written. `net.rs`'s teardown drops the handle (which aborts both loops) and then runs the
explicit flush described above.

## Known gaps

* **No port field, no game mode, no allow-commands toggle.** Vanilla's
  `MultiplayerOptionsScreen` is a form; this publishes straight away on
  `net::LAN_DEFAULT_PORT` (25565). A port already in use fails the publish loudly rather than
  sliding to another, because a host who reads "opened on 25565" and is actually on 25566 has
  been given a wrong answer.
* **The button is always present**, where vanilla hides the whole half-width row on a remote
  server. `PauseButton::enabled` is a pure function of the variant at every call site, so the
  "there is nothing of ours to publish" case is stated in chat instead.
* **The LAN world has no mob population.** `open_to_lan` seeds no `MobSim` (its own comment
  says so); the tick loop ticks whatever is there, and nothing puts anything there.
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
