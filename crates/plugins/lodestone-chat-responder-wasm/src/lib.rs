//! The worked example: a Lodestone plugin compiled to a WebAssembly component and
//! loaded from a file at runtime.
//!
//! It observes chat, counts what it has seen, and answers any message containing
//! `ping` with `pong`. That is small on purpose — the thing under test is the ABI,
//! not the plugin.
//!
//! # The counter is the point
//!
//! `SEEN` is not decoration. Issue #173 left open whether "guest state can persist
//! across calls (needed for anything resumable) or whether the host owns all state
//! and the guest is purely stateless request/response". This plugin answers it by
//! demonstration: the host keeps one `Store` per guest, so linear memory — and
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

/// How many chat messages this guest has been handed since it was loaded.
///
/// An `AtomicU64` rather than a `static mut` because the workspace denies
/// `unsafe_code`; a wasm guest is single-threaded, so the atomicity costs nothing
/// and buys the borrow checker's approval.
static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct ChatResponder;

impl Guest for ChatResponder {
    fn init() -> PluginInfo {
        log(LogLevel::Info, "chat-responder starting up");
        PluginInfo {
            name: "chat-responder".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // Must match `lodestone_wasm_host::ABI_WORLD`, or the host refuses to
            // load this plugin with a message that names both sides.
            abi: "lodestone:plugin@0.1.0".to_string(),
        }
    }

    fn on_tick(events: Vec<Event>) -> Vec<Action> {
        #[cfg(feature = "spin")]
        return spin_forever();

        #[cfg(not(feature = "spin"))]
        return respond(events);
    }
}

/// THE PREEMPTION FIXTURE. A native plugin doing this hangs the game with no
/// recourse — issue #168's honest answer for that tier. Here the host's fuel budget
/// turns it into a trap and the guest is marked permanently failed, which is the
/// isolation the wasm tier exists to provide.
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

#[cfg(not(feature = "spin"))]
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
