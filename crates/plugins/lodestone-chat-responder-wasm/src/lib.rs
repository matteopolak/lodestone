//! The worked example: a Lodestone plugin compiled to a WebAssembly component and
//! loaded from a file at runtime.
//!
//! It observes chat, counts what it has seen, and answers any message containing
//! `ping` with `pong`. That is small on purpose — the thing under test is the ABI,
//! not the plugin.
//!
//! # The counter is the point
//!
//! `SEEN` is not decoration. The WASM capability ABI has to settle whether guest
//! state can persist across calls (needed for anything resumable) or whether the
//! host owns all state and the guest is purely stateless request/response. This
//! plugin answers it by demonstration: the host keeps one `Store` per guest, so
//! linear memory — and
//! therefore this counter — survives from tick to tick, and the reply text carries
//! the count so a test can assert it from outside. A stateless request/response
//! design would make a resumable computation inside a guest impossible, which is
//! the whole reason the question mattered.

wit_bindgen::generate!({
    // The host's vendored world, by relative path. An out-of-tree plugin copies
    // `wit/lodestone-plugin.wit` next to its own source instead — the component
    // model resolves imports by name, so a byte-identical copy is all that is
    // needed, and `lodestone_wasm_host::WIT_WORLD_SOURCE` is how you get one that
    // matches the host binary you are targeting rather than whatever is on `main`.
    world: "plugin",
    path: "../../lodestone-wasm-host/wit",
});

use lodestone::plugin::logging::{log, LogLevel};
use lodestone::plugin::types::CommandSpec;
#[cfg(feature = "fs-write")]
use lodestone::plugin::filesystem_write::write_file;
#[cfg(any(feature = "inventory-click", feature = "inventory-click-invalid"))]
use lodestone::plugin::types::{InventoryClick, InventoryClickButton};
#[cfg(any(
    feature = "inventory-hotbar-swap",
    feature = "inventory-hotbar-swap-invalid"
))]
use lodestone::plugin::types::InventoryHotbarSwap;
#[cfg(any(feature = "inventory-throw", feature = "inventory-throw-invalid"))]
use lodestone::plugin::types::{InventoryThrow, InventoryThrowMode};
#[cfg(feature = "drop-selected-item")]
use lodestone::plugin::types::SelectedItemDropMode;
#[cfg(feature = "commands")]
use lodestone::plugin::types::CommandAnchor;
#[cfg(feature = "look")]
use lodestone::plugin::types::LookIntent;
#[cfg(feature = "movement")]
use lodestone::plugin::types::MovementIntent;
#[cfg(any(feature = "place", feature = "break"))]
use lodestone::plugin::types::BlockFace;
#[cfg(any(feature = "place", feature = "break", feature = "world-read"))]
use lodestone::plugin::types::BlockPos;
#[cfg(feature = "world-read")]
use lodestone::plugin::world_snapshot::read_blocks;
#[cfg(feature = "place")]
use lodestone::plugin::types::{PlaceIntent, PlaceStatus};
#[cfg(feature = "break")]
use lodestone::plugin::types::{BreakIntent, BreakStatus};
#[cfg(feature = "scheduler")]
use lodestone::plugin::scheduler::{cancel, schedule_once, schedule_repeating};

/// How many chat messages this guest has been handed since it was loaded.
///
/// An `AtomicU64` rather than a `static mut` because the workspace denies
/// `unsafe_code`; a wasm guest is single-threaded, so the atomicity costs nothing
/// and buys the borrow checker's approval.
#[cfg(not(feature = "fs-write"))]
static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "place")]
static PLACEMENT_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "break")]
static BREAK_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "drop-selected-item")]
static DROP_SELECTED_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "swap-offhand")]
static SWAP_OFFHAND_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "release-use-item")]
static RELEASE_USE_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "stab")]
static STAB_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "respawn")]
static RESPAWN_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "disconnect")]
static DISCONNECT_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "send-command")]
static COMMAND_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "scheduler")]
static REPEATS_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "scheduler")]
static ZERO_PERIOD_SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "scheduler")]
static REPEATING_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

struct ChatResponder;

impl Guest for ChatResponder {
    fn init() -> PluginInfo {
        log(LogLevel::Info, "chat-responder starting up");
        #[cfg(feature = "scheduler")]
        {
            schedule_once(2, 11);
            let repeating = schedule_repeating(1, 2, 22);
            REPEATING_ID.store(repeating, std::sync::atomic::Ordering::Relaxed);
            let cancelled = schedule_once(1, 33);
            cancel(cancelled);
            schedule_once(2, 44);
            schedule_repeating(1, 0, 66);
        }
        PluginInfo {
            name: "chat-responder".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // Must match `lodestone_wasm_host::ABI_WORLD`, or the host refuses to
            // load this plugin with a message that names both sides.
            abi: "lodestone:plugin@0.24.0".to_string(),
            commands: command_specs(),
        }
    }

    fn on_tick(events: Vec<Event>) -> Vec<Action> {
        let _ = &events;
        #[cfg(feature = "spin")]
        return spin_forever();

        #[cfg(feature = "alloc-loop")]
        return alloc_loop();

        #[cfg(feature = "network")]
        return attempt_network();

        #[cfg(feature = "look")]
        return vec![Action::SetLook(Some(LookIntent {
            yaw: 37.5,
            pitch: -12.0,
        }))];

        #[cfg(feature = "movement")]
        return vec![Action::SetMovement(Some(MovementIntent {
            forward: 1.0,
            strafe: -1.0,
            jump: true,
            sneak: true,
            sprint: true,
        }))];

        #[cfg(feature = "place")]
        return place_once_then_report(events);

        #[cfg(feature = "break")]
        return break_once_then_report(events);

        #[cfg(feature = "select-slot")]
        return vec![Action::SelectSlot(6)];

        #[cfg(feature = "select-slot-invalid")]
        return vec![Action::SelectSlot(9)];

        #[cfg(feature = "inventory")]
        return report_inventory_change(events);

        #[cfg(feature = "inventory-click")]
        return vec![Action::InventoryClick(InventoryClick {
            slot: 36,
            button: InventoryClickButton::Left,
        })];

        #[cfg(feature = "inventory-click-invalid")]
        return vec![Action::InventoryClick(InventoryClick {
            slot: u16::MAX,
            button: InventoryClickButton::Right,
        })];

        #[cfg(feature = "inventory-quick-move")]
        return vec![Action::InventoryQuickMove(36)];

        #[cfg(feature = "inventory-quick-move-invalid")]
        return vec![Action::InventoryQuickMove(u16::MAX)];

        #[cfg(feature = "inventory-double-click")]
        return vec![Action::InventoryDoubleClick(36)];

        #[cfg(feature = "inventory-hotbar-swap")]
        return vec![Action::InventoryHotbarSwap(InventoryHotbarSwap {
            slot: 36,
            hotbar: 3,
        })];

        #[cfg(feature = "inventory-hotbar-swap-invalid")]
        return vec![Action::InventoryHotbarSwap(InventoryHotbarSwap {
            slot: 36,
            hotbar: 9,
        })];

        #[cfg(feature = "inventory-throw")]
        return vec![Action::InventoryThrow(InventoryThrow {
            slot: 36,
            mode: InventoryThrowMode::Stack,
        })];

        #[cfg(feature = "inventory-throw-invalid")]
        return vec![Action::InventoryThrow(InventoryThrow {
            slot: u16::MAX,
            mode: InventoryThrowMode::One,
        })];

        #[cfg(feature = "inventory-drop-cursor")]
        return vec![Action::InventoryDropCursor];

        #[cfg(feature = "drop-selected-item")]
        return if !DROP_SELECTED_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::DropSelectedItem(SelectedItemDropMode::Stack)]
        } else {
            Vec::new()
        };

        #[cfg(feature = "swap-offhand")]
        return if !SWAP_OFFHAND_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::SwapItemWithOffhand]
        } else {
            Vec::new()
        };

        #[cfg(feature = "release-use-item")]
        return if !RELEASE_USE_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::ReleaseUseItem]
        } else {
            Vec::new()
        };

        #[cfg(feature = "stab")]
        return if !STAB_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::Stab]
        } else {
            Vec::new()
        };

        #[cfg(feature = "respawn")]
        return if !RESPAWN_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::Respawn]
        } else {
            Vec::new()
        };

        #[cfg(feature = "disconnect")]
        return if !DISCONNECT_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::Disconnect]
        } else {
            Vec::new()
        };

        #[cfg(feature = "send-command")]
        return if !COMMAND_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
            vec![Action::SendCommand("time query daytime".to_owned())]
        } else {
            Vec::new()
        };

        #[cfg(feature = "world-read")]
        return report_world_snapshot();

        #[cfg(feature = "fs-write")]
        return write_files();

        #[cfg(not(any(feature = "spin", feature = "alloc-loop", feature = "network", feature = "look", feature = "movement", feature = "place", feature = "break", feature = "select-slot", feature = "select-slot-invalid", feature = "inventory", feature = "inventory-click", feature = "inventory-click-invalid", feature = "inventory-quick-move", feature = "inventory-quick-move-invalid", feature = "inventory-double-click", feature = "inventory-hotbar-swap", feature = "inventory-hotbar-swap-invalid", feature = "inventory-throw", feature = "inventory-throw-invalid", feature = "inventory-drop-cursor", feature = "drop-selected-item", feature = "swap-offhand", feature = "release-use-item", feature = "stab", feature = "respawn", feature = "disconnect", feature = "send-command", feature = "world-read", feature = "fs-write")))]
        return respond(events);
    }

    fn on_task(id: TaskId, token: u64) -> Vec<Action> {
        #[cfg(feature = "scheduler")]
        {
            return match token {
                11 => {
                    schedule_once(0, 55);
                    vec![Action::SendChat("task: once".to_owned())]
                }
                22 => {
                    if id != REPEATING_ID.load(std::sync::atomic::Ordering::Relaxed) {
                        return vec![Action::SendChat("task: repeating id changed".to_owned())];
                    }
                    let invocation =
                        REPEATS_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if invocation == 2 {
                        cancel(id);
                    }
                    vec![Action::SendChat(format!("task: repeating {invocation}"))]
                }
                33 => vec![Action::SendChat("task: cancelled task ran".to_owned())],
                44 => vec![Action::SendChat("task: same deadline".to_owned())],
                55 => vec![Action::SendChat("task: callback scheduled".to_owned())],
                66 => {
                    let invocation =
                        ZERO_PERIOD_SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if invocation == 2 {
                        cancel(id);
                    }
                    vec![Action::SendChat(format!("task: zero period {invocation}"))]
                }
                _ => Vec::new(),
            };
        }

        #[cfg(not(feature = "scheduler"))]
        {
            let _ = (id, token);
            Vec::new()
        }
    }

    fn on_verdict(_context: VerdictContext) -> PluginVerdict {
        #[cfg(feature = "verdict-trap")]
        panic!("verdict fixture trap");

        #[cfg(feature = "verdict-deny")]
        return PluginVerdict::Deny;

        #[cfg(not(any(feature = "verdict-deny", feature = "verdict-trap")))]
        PluginVerdict::Allow
    }

    fn on_command(input: String, context: CommandContext) -> CommandOutcome {
        #[cfg(feature = "commands")]
        if input == "wasm-ping" && context.sender_name == "Console" && context.execution.is_none() {
            return CommandOutcome::Success(37);
        }

        #[cfg(feature = "commands")]
        if input == "wasm-ping" && context.sender_name == "Alex" {
            if let Some(execution) = context.execution {
                if execution.entity.is_none()
                    && execution.position.x == 12.5
                    && execution.position.y == 64.0
                    && execution.position.z == -3.25
                    && execution.rotation.yaw == 90.0
                    && execution.rotation.pitch == -15.0
                    && execution.dimension == "minecraft:overworld"
                    && execution.anchor == CommandAnchor::Eyes
                    && execution.permission_level == 3
                {
                    return CommandOutcome::Success(61);
                }
            }
            return CommandOutcome::Failure("context did not reach the guest intact".to_owned());
        }

        #[cfg(feature = "commands")]
        if input.starts_with("wasm-ping ") {
            return CommandOutcome::Failure(format!("unexpected command input: {input}"));
        }

        let _ = (input, context);
        CommandOutcome::Failure("unknown command".to_owned())
    }
}

#[cfg(feature = "world-read")]
fn report_world_snapshot() -> Vec<Action> {
    let positions = vec![
        BlockPos { x: 2, y: 60, z: 2 },
        BlockPos { x: 17, y: 60, z: 2 },
        BlockPos { x: 2, y: 320, z: 2 },
    ];
    let states = read_blocks(&positions).expect("the host must install a world snapshot");
    let states = states
        .into_iter()
        .map(|state| state.map_or("none".to_owned(), |state| state.to_string()))
        .collect::<Vec<_>>()
        .join(",");
    vec![Action::SendChat(format!("world:{states}"))]
}

/// Exercise both halves of the write contract. The parent traversal must be
/// rejected before any host filesystem mutation, while the file below the
/// configured root must be written with the exact copied bytes.
#[cfg(feature = "fs-write")]
fn write_files() -> Vec<Action> {
    let outside = write_file("../outside.txt", b"must-not-escape");
    let inside = write_file("written.txt", b"written-by-guest");
    vec![Action::SendChat(format!(
        "fs-write: outside={} inside={}",
        outside.is_ok(),
        inside.is_ok()
    ))]
}

/// Request one placement, then turn the finite next-tick lifecycle result into
/// an ordinary action. The fixture target deliberately has no loaded world in
/// its host-side gate, making `no-world-data` a stable control for the complete
/// host → ECS → shell → host observation path.
#[cfg(feature = "place")]
fn place_once_then_report(events: Vec<Event>) -> Vec<Action> {
    for event in events {
        if let Event::PlaceOutcome(outcome) = event {
            let status = match outcome.status {
                PlaceStatus::Predicted => "predicted",
                PlaceStatus::SentUnpredicted => "sent-unpredicted",
                PlaceStatus::Rejected(_) => "rejected",
            };
            return vec![Action::SendChat(format!(
                "place: generation={} status={status}",
                outcome.generation
            ))];
        }
    }
    if !PLACEMENT_SENT.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return vec![Action::PlaceBlock(PlaceIntent {
            pos: BlockPos { x: 4, y: 64, z: 4 },
            face: BlockFace::Up,
        })];
    }
    Vec::new()
}

/// Start one persistent dig, observe the shell's bounded result, then explicitly
/// release ownership. The fixture deliberately targets absent world data, so the
/// shell's normal ray validation deterministically rejects it while still
/// exercising the production consumer.
#[cfg(feature = "break")]
fn break_once_then_report(events: Vec<Event>) -> Vec<Action> {
    for event in events {
        if let Event::BreakOutcome(outcome) = event {
            let status = match outcome.status {
                BreakStatus::Idle => "idle",
                BreakStatus::Progressing => "progressing",
                BreakStatus::Rejected(_) => "rejected",
            };
            return vec![
                Action::SetBreak(None),
                Action::SendChat(format!("break: status={status}")),
            ];
        }
    }
    if !BREAK_STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return vec![Action::SetBreak(Some(BreakIntent {
            pos: BlockPos { x: 4, y: 64, z: 4 },
            face: BlockFace::Up,
        }))];
    }
    Vec::new()
}

/// Report the copied inventory identity received through the guest event batch.
/// This fixture deliberately observes instead of reaching for any inventory
/// handle, so its output distinguishes an event granted by the host from a
/// parallel inventory state that happened to be present in the test app.
#[cfg(feature = "inventory")]
fn report_inventory_change(events: Vec<Event>) -> Vec<Action> {
    for event in events {
        if let Event::InventorySlotChanged(change) = event {
            let item = change.item.map_or_else(
                || "empty".to_owned(),
                |item| format!("{}x{}", item.item, item.count),
            );
            return vec![Action::SendChat(format!(
                "inventory: slot={} item={item}",
                change.slot
            ))];
        }
    }
    Vec::new()
}

fn command_specs() -> Vec<CommandSpec> {
    #[cfg(feature = "commands")]
    {
        vec![CommandSpec {
            name: "wasm-ping".to_owned(),
            description: "Proves a guest-owned command reached the runtime host.".to_owned(),
            aliases: vec!["wp".to_owned()],
            permission: Some("wasm.command.use".to_owned()),
        }]
    }

    #[cfg(not(feature = "commands"))]
    Vec::new()
}

/// THE PREEMPTION FIXTURE. A native plugin doing this hangs the game with no
/// recourse: a panicking native plugin runs on the same thread and inside the
/// same schedule call as every internal system, with no isolation boundary. Here
/// the host's fuel budget turns it into a trap and the guest is marked
/// permanently failed, which is the isolation the wasm tier exists to provide.
///
/// `black_box` keeps the optimiser from deleting an observably-pure infinite loop.
#[cfg(feature = "spin")]
fn spin_forever() -> ! {
    let mut n: u64 = 0;
    loop {
        n = n.wrapping_add(1);
        std::hint::black_box(n);
    }
}

/// THE MEMORY-CEILING FIXTURE. Grows linear memory in fixed chunks until the
/// host's `ResourceLimiter` denies the next `memory.grow`, reporting how far it
/// got before that happened. `try_reserve_exact` rather than plain allocation:
/// a denied `memory.grow` makes the global allocator return null, and an
/// *infallible* allocation turns that into `handle_alloc_error` — an abort, which
/// would make this indistinguishable from the preemption fixture and would not
/// let a single tick report a byte count. `try_reserve_exact` surfaces the same
/// denial as an ordinary `Err`, so the guest can measure its own ceiling instead
/// of merely dying at it.
///
/// Capacity only, never written: the point under test is `memory.grow`, which the
/// allocator calls when it needs more pages regardless of whether the bytes are
/// ever initialised, so there is no reason to pay fuel for a fill.
#[cfg(feature = "alloc-loop")]
fn alloc_loop() -> Vec<Action> {
    const CHUNK: usize = 4 * 1024 * 1024;
    // 512 * 4 MiB = 2 GiB, comfortably past any `with_memory_limit` value the
    // gate configures, so the loop is guaranteed to hit either the host's ceiling
    // or wasm32's own 4 GiB linear-memory address-space limit, never its own cap.
    const MAX_CHUNKS: usize = 512;

    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut grown: usize = 0;
    let mut denied = false;
    for _ in 0..MAX_CHUNKS {
        let mut chunk: Vec<u8> = Vec::new();
        match chunk.try_reserve_exact(CHUNK) {
            Ok(()) => {
                grown += CHUNK;
                chunks.push(chunk);
            }
            Err(_) => {
                denied = true;
                break;
            }
        }
    }
    vec![Action::SendChat(format!("alloc: bytes={grown} denied={denied}"))]
}

/// THE NETWORK-DENIAL FIXTURE. Attempts one real TCP connect. There is no
/// `lodestone:plugin` import for this — the WIT world defines no sockets
/// interface at all — so this is not exercising a capability the host might
/// grant; it is exercising `wasm32-unknown-unknown`'s own `std::net`, whose
/// entire implementation is `sys::unsupported` (see `src/host.rs`'s header: "no
/// clock, no socket … for a guest to find"). The point of running it here rather
/// than reasoning about it from the platform docs is that a *test* is evidence
/// and a claim about the standard library is not.
#[cfg(feature = "network")]
fn attempt_network() -> Vec<Action> {
    // Must match `NETWORK_PROBE_ADDR` in
    // `crates/lodestone-wasm-host/tests/network_denial.rs`. Duplicated rather than
    // shared: this crate is deliberately not importable from the native
    // workspace (its `Cargo.toml` header explains why a `cdylib` guest cannot be
    // a normal path/git dependency of a workspace member).
    let addr = "127.0.0.1:47899";
    let result = std::net::TcpStream::connect(addr);
    vec![Action::SendChat(format!(
        "net: ok={} err={}",
        result.is_ok(),
        result.err().map(|e| e.to_string()).unwrap_or_default()
    ))]
}

#[cfg(not(any(feature = "spin", feature = "alloc-loop", feature = "network", feature = "look", feature = "movement", feature = "drop-selected-item", feature = "fs-write")))]
fn respond(events: Vec<Event>) -> Vec<Action> {
    {
        let mut actions = Vec::new();
        for event in events {
            let Event::Chat(chat) = event else {
                // Not silently dropped in the island sense: the other arms are
                // events this plugin declared no interest in, and the host does
                // not deliver an event a manifest did not subscribe to. Reaching
                // here at all means the manifest asked for more than this code
                // handles, which is the plugin author's own business.
                continue;
            };
            let seen = SEEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if chat.text.to_lowercase().contains("ping") {
                actions.push(Action::SendChat(format!(
                    "pong (chat messages seen: {seen})"
                )));
            }
        }

        #[cfg(feature = "misbehave")]
        {
            // THE DENY-CASE FIXTURE. Reached only under `--features misbehave`,
            // which builds a second artifact from this same source. With no
            // `fs:read` capability the `lodestone:plugin/filesystem` interface is
            // absent from the host's `Linker` and the component fails to
            // *instantiate* — these lines never run. That is the assertion: not
            // that a well-behaved plugin does not try, but that a plugin which
            // does try cannot get in.
            //
            // Two reads, because the host's defence has two layers and the test
            // asserts both. `/etc/passwd` is outside any filesystem root the host
            // would configure, so even a *granted* plugin is refused it;
            // `granted.txt` is inside, so a granted plugin really does get bytes.
            // Without the second read, "granting the capability changes nothing
            // observable" would be indistinguishable from "the capability works".
            let stolen = crate::lodestone::plugin::filesystem::read_file("/etc/passwd");
            log(LogLevel::Error, &format!("outside-root read: {stolen:?}"));
            let allowed = crate::lodestone::plugin::filesystem::read_file("granted.txt");
            log(LogLevel::Error, &format!("inside-root read: {allowed:?}"));
            actions.push(Action::SendChat(format!(
                "fs: outside={} inside={}",
                stolen.is_ok(),
                allowed.map_or_else(|e| format!("Err({e})"), |b| String::from_utf8_lossy(&b).into_owned())
            )));
        }

        actions
    }
}

export!(ChatResponder);
