# Plugin player game-mode control

## What it is

`IntegratedServer::set_player_game_mode_proposed` is the native plugin bridge for changing one connected player's game mode. It resolves a typed proposal before queuing a copied effect, then lets the target connection apply the change and emit the protocol-specific state.

## How it works

The caller submits a `ServerProposalAction::SetPlayerGameMode` containing only a `Uuid` and `GameMode` through `ServerProposalHandle`. The server tick owns the proposal queue and publishes the action to `TickSet::Adjudicate`; a native plugin can allow, deny, or replace it. A replacement must still be a player game-mode action or the API returns `MismatchedAction`.

After adjudication, `IntegratedServer` checks the resolved target against the shared `PlayerRegistry` and queues `Effect::SetGameMode`. The target connection drains that directed queue during its normal sync pass. Its existing effect consumer updates the connection-local mode and abilities, republishes the mode to the shared roster, and calls the selected `ServerProtocol` encoders. No ECS world, guard, or internal entity handle crosses the callback boundary.

## How to change it

Keep the proposal payload version-free and owned by copied values. Extend `ServerProposalAction`, the proposal handle, and the exhaustive proposal consumers together. Preserve the order: adjudicate first, queue second, encode only in the target connection. `push_effect` returns `false` when a player disconnects between adjudication and delivery; this is surfaced as `PlayerGameModeRefusal::UnknownPlayer` and must not leave a queued effect for a later join.

The action is intentionally not a Paper-shaped event yet: its native plugin policy is the authoritative gate, while `PaperEventBus` leaves unsupported proposal variants untouched. Add a separate event shape only when a concrete event owner and consumer exist.

## Configuration

Native plugins install an adjudication system with `ServerApp::bootstrap_with` and place it in `TickSet::Adjudicate`. The proposal ingress is bounded and each request has a one-second response deadline. A server without a running shared tick task returns `PlayerGameModeRefusal::Unavailable`.

## Dependencies

The bridge depends on `lodestone-server::ecs` proposal resources, `PlayerRegistry`'s directed effect queue, `commands::Effect`, and the `ServerProtocol` game-mode and abilities encoders. The wire result is produced by the existing `server::apply_own_effect` consumer.
