//! The wasm-safe **player controller**: the platform-independent pipeline from
//! held keys to the outbound movement action, shared by the native (winit)
//! `lodestone-shell` and the browser (web-sys) `web/` client so movement has
//! exactly **one** implementation.
//!
//! ```text
//! platform key/mouse events            (winit  OR  web-sys — the only per-platform layer)
//!   → RawInput(InputState)             (this crate, an ECS resource)
//!   → movement_intent / apply_look     (this crate)
//!   → MovementIntent component         (lodestone-ecs, written in TickSet::Input)
//!   → lodestone_physics::tick          (bit-exact, shared; TickSet::Physics)
//!   → move_action → ClientAction::Move (this crate, queued in TickSet::Send)
//!   → ClientHandle::send_action        (lodestone-client, shared)
//! ```
//!
//! Since Stage 2 of [`docs/bevy-migration.md`] that pipeline is *systems*, in
//! [`ecs`]. The pure functions ([`movement_intent`], [`apply_look`],
//! [`move_action`], [`swim_adjusted_intent`]) are unchanged and are what the
//! systems call, so a caller with no `App` — the browser client today — can
//! still drive movement directly.
//!
//! [`docs/bevy-migration.md`]: https://github.com/
//!
//! Two internally-consistent movement implementations is how bit-exact physics
//! parity dies quietly — no test catches it, because each path agrees with
//! itself and only one agrees with vanilla. Extracting this core means neither
//! platform can grow its own [`move_action`] lowering or its own sprint/look
//! rules.
//!
//! ## wasm safety is enforced, not assumed
//!
//! This crate depends only on `lodestone-physics`, `lodestone-client`,
//! `lodestone-ecs`, `lodestone-model` and `bevy_ecs`/`bevy_app` — all wasm-safe
//! (`bevy_ecs` proven so by Stage 0's `scripts/wasm-check.sh` run, never
//! `multi_threaded`, which does not compile on a threadless wasm target) — and
//! itself touches no clock, filesystem, socket, or thread API.
//! [`std::time::Instant::now`] and `SystemTime::now` *compile* on
//! `wasm32-unknown-unknown` but **panic at runtime**, and a `cfg` cannot turn a
//! fresh call to one into a compile error (§12.35), so the
//! the `no_wasm_trap_symbols_are_confined` guard test scans this crate's
//! own source and fails if any appears — the durable half of wasm safety, the
//! same technique `lodestone-render`'s `FramePacer`/`TimeSource` uses. Frame
//! pacing itself is not this crate's job: the tick/render clock split lives in
//! the platform layer, which injects time (native `Instant`, browser
//! `performance.now()`) rather than letting this crate name a clock.

mod action;
pub mod ecs;
mod input;

pub use action::move_action;
pub use ecs::{ControllerPlugin, RawInput, swim_adjusted_intent};
pub use input::{
    Action, InputState, PITCH_LIMIT, SPRINT_TRIGGER_WINDOW_TICKS, apply_look,
    apply_look_inverted, movement_intent, sensitivity_factor,
};

#[cfg(test)]
mod guard {
    /// Fail if any wasm-runtime-trap or non-wasm-portable symbol appears in this
    /// crate's source. The controller is shared with the browser, so a fresh
    /// `Instant::now()`/`SystemTime::now()` (compiles green, panics in a browser)
    /// or an `std::fs`/`std::net`/`std::thread::spawn`/`tokio::time`/`mio` call
    /// (won't build for wasm at all) must be rejected here rather than discovered
    /// when the web build breaks. The allow-list is intentionally empty: this
    /// crate needs none of them.
    #[test]
    fn no_wasm_trap_symbols_are_confined() {
        use std::fs;
        use std::path::PathBuf;

        // Built by concatenation so this guard file does not match itself.
        let instant_now = format!("Instant{}", "::now");
        let systemtime = format!("System{}", "Time");
        let fs_call = format!("std::{}::", "fs");
        let net_mod = format!("std::{}", "net");
        let thread_spawn = format!("std::thread::{}", "spawn");
        let tokio_time = format!("tokio::{}", "time");
        let reactor_dep = format!("mi{}", "o");
        // (banned pattern, files where it is permitted — none here)
        let rules: [(&str, &[&str]); 7] = [
            (instant_now.as_str(), &[]),
            (systemtime.as_str(), &[]),
            (fs_call.as_str(), &[]),
            (net_mod.as_str(), &[]),
            (thread_spawn.as_str(), &[]),
            (tokio_time.as_str(), &[]),
            (reactor_dep.as_str(), &[]),
        ];

        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        let mut files = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).expect("read_dir src") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        assert!(!files.is_empty(), "guard found no source files to scan");

        let mut violations = Vec::new();
        for path in &files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = fs::read_to_string(path).expect("read source");
            for (lineno, raw) in text.lines().enumerate() {
                // Drop line/doc comments so documentation may name the symbols.
                let code = raw.split("//").next().unwrap_or("");
                for (pat, allowed) in &rules {
                    if code.contains(pat) && !allowed.contains(&name.as_str()) {
                        violations.push(format!(
                            "{}:{}:{}",
                            path.display(),
                            lineno + 1,
                            raw.trim()
                        ));
                    }
                }
            }
        }
        assert!(
            violations.is_empty(),
            "wasm runtime-trap / non-portable symbols found in the shared controller \
             (these break or panic in a browser):\n{}",
            violations.join("\n")
        );
    }
}
