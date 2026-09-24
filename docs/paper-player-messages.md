# Paper player messages

## What it is

`PlayerRegistry::send_message` is the bounded player-object message operation. It queues one exact system-chat line for one connected UUID, matching the useful `Player.sendMessage` compatibility slice without exposing a connection or protocol object.

## How it works

The method resolves the target under the registry lock and appends `Effect::Message` only when that UUID is currently registered. The serving connection is the sole reader: its `apply_own_effect` path drains the directed queue and calls the active protocol's system-chat encoder. A disconnected or unknown UUID returns `false` and cannot leave a process-lifetime queue entry behind.

The operation is intentionally directed. It does not use `PlayerRegistry::say`, whose shared cursor delivers a broadcast line to every connection, and it does not retain a player handle across a disconnect.

## How to change it

Keep `send_message` as a thin request into `PlayerRegistry::push_effect`; the connection loop remains responsible for protocol output. Add a new `Effect` variant only when the operation needs state or wire behavior that a system-chat line cannot represent. Preserve the unknown-UUID negative behavior and exact message text in the registry tests.

## Configuration

There are no environment variables or flags. The queue is bounded by connected-player registration and is removed when the player ticket is dropped.

## Dependencies

The API depends on `PlayerRegistry` and `commands::Effect` in `lodestone-server`. The existing connection loop and `ServerProtocol::encode_system_chat` provide the production egress; no JVM or Paper jar is required for the native Rust surface.
