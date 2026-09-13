//! Joins a **real third-party plugin server** and asserts the session survives
//! its lobby inventory.
//!
//! # Why this gate exists and why it is `#[ignore]`d
//!
//! Every other item-component gate in this crate builds or replays bytes. None of
//! them can see the thing that actually killed sessions here: a *plugin* server
//! stamping `minecraft:custom_data` on every hotbar slot. Vanilla never does, so
//! no vanilla oracle — container, capture or otherwise — produces the input. The
//! outside source for this gate is therefore a server nobody in this repo
//! controls, which is exactly its value and exactly why it cannot run in CI.
//!
//! It is `#[ignore]`d **and** gated on `LODESTONE_LIVE_SERVER`, so it is inert
//! twice over and no host is baked into the tree:
//!
//! ```text
//! LODESTONE_LIVE_SERVER=host:port \
//!   cargo test -p lodestone-v26-2 --test live_plugin_server_join -- --ignored --nocapture
//! ```
//!
//! # What it asserts, and what it deliberately does not
//!
//! It reaches the play phase, reads the inventory the server pushes, and requires
//! the session to still be running. Nothing is sent beyond what the client's own
//! join sequence sends: no chat, no movement, no interaction, no world edit. The
//! run lasts seconds and then disconnects.
//!
//! The assertion that matters is the **negative** one — the session did not end —
//! so it carries its own detector: `has_unmodeled` across every stack received is
//! reported, and a session that ends is reported with
//! [`lodestone_client::ClientError::cause_chain`] rather than as a bare failure,
//! because the whole point is which layer died.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::{ClientEvent, ItemStack};
use lodestone_v26_2::adapter;

/// The server to join, as `host:port`. Absent means "skip": a live gate that
/// silently *passes* against no server is the precondition species of vacuous
/// test, so this prints why it skipped.
fn live_server() -> Option<ServerAddress> {
    let raw = std::env::var("LODESTONE_LIVE_SERVER").ok()?;
    let (host, port) = raw.rsplit_once(':')?;
    Some(ServerAddress {
        host: host.to_owned(),
        port: port.parse().ok()?,
    })
}

fn describe(stack: &ItemStack) -> String {
    format!(
        "{} x{}{}{}",
        stack.item,
        stack.count,
        if stack.components.has_unmodeled {
            " [PARTIAL]"
        } else {
            ""
        },
        match &stack.components.custom_data {
            Some(bytes) => format!(" custom_data={}B", bytes.len()),
            None => String::new(),
        }
    )
}

/// The gate. Joins, drains events for a fixed window, and requires the session to
/// outlive its own inventory.
#[tokio::test]
#[ignore = "joins a real third-party server; set LODESTONE_LIVE_SERVER=host:port"]
async fn a_plugin_server_lobby_inventory_does_not_end_the_session() {
    let Some(address) = live_server() else {
        panic!(
            "set LODESTONE_LIVE_SERVER=host:port to run this gate. It is not \
             skipped silently on purpose: a live gate that passes with no server \
             attached measures nothing."
        );
    };
    let host = format!("{}:{}", address.host, address.port);

    let profile = LoginProfile {
        // An offline-identity join. A username is derived per run because an
        // offline-mode server keys the account UUID off the *name*, so a shared
        // name shares one persisted player file across runs.
        username: format!("Lodestone{:04}", std::process::id() % 10_000),
        uuid: uuid::Uuid::new_v4(),
    };
    let username = profile.username.clone();

    let (mut handle, mut events) = ClientBuilder::new(address, profile, Box::new(adapter()))
        .connect_timeout(Some(Duration::from_secs(15)))
        .read_timeout(Some(Duration::from_secs(30)))
        .connect()
        .await
        .unwrap_or_else(|error| panic!("could not reach {host}: {}", error.cause_chain()));

    // Drain for a fixed window rather than waiting on one event: which inventory
    // packet a plugin sends (`container_set_content`, `set_player_inventory`, a
    // burst of `container_set_slot`) is server-side plugin logic, not protocol,
    // so nothing here may assume a particular one.
    let mut stacks: Vec<String> = Vec::new();
    let mut partial = 0usize;
    let mut with_custom_data = 0usize;
    let mut ended: Option<String> = None;

    let window = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = window.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Err(_) => break,
            Ok(None) => {
                ended = Some("the event stream closed".to_owned());
                break;
            }
            Ok(Some(event)) => {
                let mut record = |stack: &Option<ItemStack>| {
                    if let Some(stack) = stack {
                        if stack.components.has_unmodeled {
                            partial += 1;
                        }
                        if stack.components.custom_data.is_some() {
                            with_custom_data += 1;
                        }
                        stacks.push(describe(stack));
                    }
                };
                match &event {
                    ClientEvent::ContainerContent {
                        items,
                        carried_item,
                        ..
                    } => {
                        for item in items {
                            record(item);
                        }
                        record(carried_item);
                    }
                    ClientEvent::ContainerSlot { item, .. }
                    | ClientEvent::InventorySlotChanged { item, .. }
                    | ClientEvent::CursorItemChanged { item, .. } => record(item),
                    ClientEvent::EntityEquipmentUpdated { equipment, .. } => {
                        for slot in equipment {
                            record(&slot.item);
                        }
                    }
                    ClientEvent::SessionFailed { reason } => {
                        ended = Some(format!("session failed: {reason}"));
                        break;
                    }
                    ClientEvent::Disconnect { reason } => {
                        ended = Some(format!(
                            "server disconnected us: {}",
                            reason.to_plain_string()
                        ));
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    println!("--- joined {host} as {username}");
    println!("--- {} stacks received:", stacks.len());
    for stack in &stacks {
        println!("      {stack}");
    }
    println!("--- {partial} partial, {with_custom_data} carrying custom_data");

    let alive = !handle.is_finished();
    handle.shutdown();

    assert!(
        ended.is_none() && alive,
        "the session must outlive the lobby inventory: {}",
        ended.unwrap_or_else(|| "the driver task ended".to_owned())
    );
    // The detector for the assertion above. Without a single stack there is no
    // inventory to have survived, so a green result would mean nothing.
    assert!(
        !stacks.is_empty(),
        "no item stack arrived in the window, so nothing was measured — the \
         server may have put us on a screen that sends no inventory"
    );
    assert_eq!(
        partial, 0,
        "every stack decoded whole. A non-zero count is not a failure of the \
         session (it survived) but it names the components still missing: run \
         with --nocapture and read the warnings"
    );
}
