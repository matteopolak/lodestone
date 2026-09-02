//! **The built-in commands, end to end, with no host installed.**
//!
//! `lodestone_server::commands::ServerCommands` was an island: `mod commands;`
//! was declared, its own tests were green, and `grep ServerCommands` outside
//! that one file returned nothing. Its module doc claimed `server.rs`'s
//! `ChatCommand` arm consulted it; that claim was stale, and since every real
//! constructor passes `CommandDispatch::none()`, **`/gamerule` typed by a player
//! did nothing at all**.
//!
//! Only a gate that starts at a real frame on a real wire and ends at an
//! observable effect can see that. A test that builds a `ServerCommands` and
//! calls `run` proves the executor works and says nothing about whether a player
//! can reach it — which is exactly what the old module's tests did.
//!
//! # What is real here, and the one thing that is deliberately absent
//!
//! | piece | the real thing |
//! |---|---|
//! | the sender | `lodestone-client`'s `ClientHandle::command`, driving the real `V770Adapter` encoder |
//! | the frame | protocol 776 `chat_command` (id 7), length-prefixed on a real `Connection` |
//! | the server | `V770ServerProtocol` + `lodestone_server::serve_connection_with_commands` |
//! | the dispatcher | the real `ServerCommands` tree built by `crate::commands::{gamerule,gamemode,give}` |
//! | the effect | a real clientbound `game_event`/`container_set_slot`, decoded by the real client |
//!
//! The absent thing is the **host sink**: every run here installs
//! `CommandDispatch::none()`. That is the shipping configuration, and it is the
//! configuration in which the island was invisible — with a sink installed, a
//! command reaching the sink instead of the built-ins looks like it worked.
//!
//! `V770ServerProtocol` is the real `ServerProtocol`, not a test double. A gate
//! against a double is the *world*-species vacuity this repo has been burned by
//! twice: a double complete enough to pass verifies the double.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ClientEvent, GameMode, Text};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    CommandDispatch, NoEntities, PlayerRegistry, WorldgenChunkSource, serve_connection_with_commands,
};
use lodestone_v770::{V770ServerProtocol, adapter};
use lodestone_worldgen::density::Density;

fn profile(name: &str) -> LoginProfile {
    LoginProfile { username: name.into(), uuid: uuid::Uuid::from_u128(0x5eed_0001) }
}

fn address() -> ServerAddress {
    ServerAddress { host: "memory".into(), port: 0 }
}

/// Deterministic, noise-free terrain — content is irrelevant, but the vertical
/// extent must be the real overworld shape or the client's decode misaligns.
/// Same source `command_wire_path.rs` and `server_liveness.rs` use.
fn cheap_source() -> WorldgenChunkSource {
    WorldgenChunkSource::new(
        Density::YClampedGradient { from_y: -64.0, to_y: 64.0, from_value: 1.0, to_value: -1.0 },
        -64,
        384,
    )
}

fn plain(text: &Text) -> String {
    text.to_plain_string()
}

/// What one end-to-end run observed on the *client* side.
#[derive(Debug, Default)]
struct Observed {
    chat: Vec<String>,
    game_modes: Vec<GameMode>,
    slots: Vec<(i32, i32, Option<lodestone_model::ItemStack>)>,
}

/// Joins a real client to a real server with **no host sink**, sends each
/// command as a real `chat_command` frame, and collects what came back.
///
/// `expected_events` bounds the wait: the loop stops as soon as it has that many
/// non-welcome observations. A bounded wait rather than an unbounded one, because
/// a test that hangs when the wire is broken is a worse failure report than one
/// that returns an empty `Observed`.
async fn run(commands: &[&str], expected_events: usize) -> Observed {
    let (client_end, server_end) = memory_pair();
    let source = cheap_source();
    // A real shared registry, so the caller is a real roster entry and `@s`
    // resolves through `PlayerRegistry::candidates` rather than through the
    // synthesised singleplayer fallback. Both paths matter and this is the one an
    // integrated LAN server takes.
    let players = PlayerRegistry::new();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_commands(
            &mut conn,
            &V770ServerProtocol,
            &source,
            &lodestone_server::PlayerAwareSource::new(NoEntities, players),
            0,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            // The load-bearing part: no sink. If the built-ins were still an
            // island, every command below would be answered by
            // `UNKNOWN_COMMAND`.
            &CommandDispatch::none(),
        )
        .await;
    });

    let (mut handle, mut events) =
        ClientBuilder::new(address(), profile("Commander"), Box::new(adapter()))
            .connect_with(client_end);

    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    for command in commands {
        handle.command(*command).expect("send the command");
    }

    let mut observed = Observed::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline
        && observed.chat.len() + observed.game_modes.len() + observed.slots.len() < expected_events
    {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::Chat { text, .. })) => {
                let line = plain(&text);
                if line != "Welcome to Lodestone" {
                    observed.chat.push(line);
                }
            }
            Ok(Some(ClientEvent::GameModeChanged { game_mode })) => {
                observed.game_modes.push(game_mode);
            }
            Ok(Some(ClientEvent::ContainerSlot { window_id, slot, item, .. })) => {
                observed.slots.push((window_id, slot, item));
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }

    handle.shutdown();
    server.abort();
    observed
}

/// **The island gate.** `/gamerule` typed by a player changes a rule and the
/// player is told, with no host sink installed.
///
/// This is the command that provably did nothing before: the previous
/// `ChatCommand` arm recognised only `/gamemode` and handed everything else to a
/// sink that is `None` in every shipping configuration.
///
/// The expected line is predicted exactly, not matched loosely, so a reply that
/// merely arrives is not enough.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gamerule_typed_in_chat_reaches_the_builtin_tree_with_no_host_sink() {
    let observed = run(&["gamerule random_tick_speed 6"], 1).await;
    assert_eq!(
        observed.chat,
        ["Gamerule random_tick_speed is now set to: 6"],
        "with no sink installed this used to be `Unknown or incomplete command`"
    );
}

/// The negative control for the gate above: an unknown root still falls through
/// to the (absent) host sink and is refused, so the assertion above is about the
/// built-in tree matching and not about the arm answering everything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_root_still_falls_through_to_the_absent_host_sink() {
    let observed = run(&["warp spawn"], 1).await;
    assert_eq!(
        observed.chat,
        [lodestone_server::UNKNOWN_COMMAND],
        "a root the built-ins do not own must reach the sink, which refuses"
    );
    // And the discriminating case: a *known* root with a bad argument must be
    // answered by the tree, not by the sink. Reporting `UNKNOWN_COMMAND` here
    // would tell the player the command does not exist when only their value was
    // wrong.
    let observed = run(&["gamerule random_tick_speed banana"], 1).await;
    assert_eq!(observed.chat.len(), 1);
    assert_ne!(
        observed.chat[0], lodestone_server::UNKNOWN_COMMAND,
        "a bad argument to a known root must not read as an unknown command"
    );
}

/// **`/gamemode` end to end**: the real command changes the real connection's
/// mode, and the *client* sees it.
///
/// `GameModeChanged` is decoded from a real `game_event` frame, so this cannot
/// pass on a server that updated its own local and sent nothing — which is the
/// failure an assertion on the chat line alone would miss.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gamemode_changes_the_mode_on_the_wire_and_confirms_it_in_chat() {
    // Two observations expected: the `game_event` and the confirmation line.
    let observed = run(&["gamemode creative"], 2).await;
    assert_eq!(
        observed.game_modes,
        [GameMode::Creative],
        "the client must receive a real game-mode change, not just a chat line"
    );
    assert_eq!(observed.chat, ["Set own game mode to Creative Mode"]);
}

/// 26.2 accepts the four full mode names and **nothing else** — `vanilla's own game type's own by name`
/// is an exact match against `getSerializedName`.
///
/// This is the gate on the faithfulness bug the deleted `parse_gamemode_command`
/// had: it accepted `c` and `1`, which is *more* permissive than vanilla. Its own
/// test asserted that permissiveness, so nothing was red.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gamemode_rejects_the_abbreviations_the_hand_rolled_parser_accepted() {
    let observed = run(&["gamemode c"], 1).await;
    assert!(
        observed.game_modes.is_empty(),
        "`gamemode c` must not change the mode: {observed:?}"
    );
    assert_eq!(observed.chat.len(), 1, "the player must be told why: {observed:?}");
    assert_ne!(
        observed.chat[0],
        lodestone_server::UNKNOWN_COMMAND,
        "a bad mode name is a parse error against a known root, not an unknown command"
    );

    // The control on the same wire: the full name works, so the refusal above is
    // about the abbreviation and not about `/gamemode` being unreachable.
    let observed = run(&["gamemode adventure"], 2).await;
    assert_eq!(observed.game_modes, [GameMode::Adventure]);
}

/// **`/give` end to end**: the item lands in the real inventory and the real
/// `container_set_slot` frame reaches the client.
///
/// The slot is predicted exactly. A fresh player's inventory is empty, so
/// `vanilla's own inventory's own get free slot` places the first stack in native slot `0`, which
/// `window_zero_menu_slot` maps to **menu slot 36** — the first hotbar cell.
/// Both the count and the item id are predicted too, so a `/give` that arrived
/// with the wrong count or as the wrong item fails.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn give_puts_the_item_in_the_inventory_and_syncs_the_slot() {
    let observed = run(&["give @s minecraft:diamond 3"], 2).await;
    assert_eq!(observed.chat, ["Gave 3 minecraft:diamond to Commander"]);

    let (window, slot, item) = observed
        .slots
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("no container_set_slot arrived: {observed:?}"));
    assert_eq!(window, 0, "the player's own inventory is window 0");
    assert_eq!(slot, 36, "native 0 is menu slot 36, the first hotbar cell");
    let item = item.expect("the slot must carry an item");
    assert_eq!(item.item.to_string(), "minecraft:diamond");
    assert_eq!(item.count, 3, "the count must be the one asked for");
}

/// `/give` with no count defaults to 1 — the *shallow* of the two executable
/// nodes on one path, which is the shape that would be wrong if the optional
/// trailing argument had been modelled as an `Option<T>` parameter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn give_without_a_count_gives_exactly_one() {
    let observed = run(&["give @s minecraft:diamond"], 2).await;
    assert_eq!(observed.chat, ["Gave 1 minecraft:diamond to Commander"]);
    let item = observed
        .slots
        .first()
        .and_then(|(_, _, item)| item.clone())
        .unwrap_or_else(|| panic!("no slot arrived: {observed:?}"));
    assert_eq!(item.count, 1);
}

/// An unknown item is refused at the *tree*, so no slot is ever written — and
/// the control that the identical command with a real item does write one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn give_refuses_an_unknown_item_before_touching_the_inventory() {
    let observed = run(&["give @s minecraft:diamnod 3"], 1).await;
    assert!(observed.slots.is_empty(), "nothing may be written: {observed:?}");
    assert_eq!(observed.chat.len(), 1);

    let observed = run(&["give @s minecraft:diamond 3"], 2).await;
    assert!(!observed.slots.is_empty(), "the control must write a slot");
}

/// The component-patch refusal is a *parse* refusal reaching the player, not a
/// silent drop of the components.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn give_with_a_component_patch_says_components_are_unsupported() {
    let observed = run(&["give @s minecraft:diamond_sword[minecraft:damage=5]"], 1).await;
    assert!(observed.slots.is_empty(), "no item may be given: {observed:?}");
    assert_eq!(observed.chat.len(), 1);
    assert!(
        observed.chat[0].contains("components are not supported"),
        "the refusal must name components: {observed:?}"
    );
}
