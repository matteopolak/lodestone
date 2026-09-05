# Multiplayer connections and join diagnostics

## What it is

The multiplayer connection path turns a saved or command-line server address into a concrete TCP endpoint while preserving the address presented during the protocol handshake. It also separates failures to establish TCP from silence after a session has started.

## How it works

Movement reconciliation has a dedicated `net_join` tracing target. It records
each decoded server correction with its raw relative flags and resolved pose,
each correction adopted by the simulation, every encoded outbound movement
packet, and any explosion or direct velocity impulse applied to the local
predicted velocity. Run
`RUST_LOG=warn,net_join=debug just run` for a short reproduction without enabling
unrelated renderer or shader diagnostics.

A saved server entry retains its port as `Option<u16>`. An explicit port is dialed unchanged. A bare hostname is passed to `lodestone_net::resolve_server_address`, which checks `_minecraft._tcp.<host>` and uses the selected SRV target when present, otherwise falling back to port `25565`.

The resolved host and port are supplied through `ClientBuilder::connect_target`; the original `ServerAddress` remains untouched for the handshake. This distinction matters for virtual-hosting proxies, which route using the hostname the player entered even when DNS directs the socket elsewhere.

TCP establishment has a 10-second budget and reports `ClientError::ConnectTimeout`. Once connected, the packet reader has a separate 30-second idle budget and reports `ClientError::Timeout`. A read timeout logs the current protocol state, received-packet count, and last packet ID under the `net_join` target.

## How to change it

Change address selection in `lodestone-net::resolve`; keep the saved entry's optional port intact until that function is called. Change socket-versus-handshake routing through `ClientBuilder::connect_target`, not by replacing `ServerAddress`. If the loading screen needs another phase, update `NetUpdate::ConnectPhase` and `menu::loading::ConnectPhase` together.

The packet ID in a timeout diagnostic is state-relative: interpret it together with the logged `ConnectionState` and the selected protocol adapter. Do not treat the same numeric ID as one packet across protocol families or states.

## Configuration

The shell currently fixes TCP connection timeout at 10 seconds and inbound packet idle timeout at 30 seconds in `lodestone_shell::net`. For focused join logs without GPU or shader compiler noise, run:

```text
RUST_LOG=warn,net=info,net_join=info just run
```

An explicitly entered port suppresses SRV lookup. A bare hostname enables it.

## Dependencies

Address resolution uses `lodestone-net` and the system DNS configuration through `hickory-resolver`. Session startup and timeout errors come from `lodestone-client`; `lodestone-shell` owns the server-list entry, loading screen, and focused tracing targets.
