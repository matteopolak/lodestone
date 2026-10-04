//! World preparation shared by the live gates that need an exact block
//! arrangement: the gate builds its fixture over RCON after it has joined,
//! instead of depending on whatever an earlier run left in the oracle world.

use std::time::{Duration, Instant};

use lodestone_testsupport::RconClient;

/// The flat creative oracle's RCON endpoint (`scripts/live-oracles/creative.sh`).
pub const CREATIVE_RCON: &str = "127.0.0.1:25571";
pub const RCON_PASSWORD: &str = "lodestone";

/// Connects to an oracle's RCON, failing with the launcher that provides it.
pub fn connect(addr: &str, launcher: &str) -> RconClient {
    RconClient::connect(addr, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!("cannot reach RCON at {addr}: {e}. Fix: start the oracle with `{launcher}`.")
    })
}

/// Runs every command and fails if the server's reply says it was refused.
///
/// An empty reply is accepted (a command that succeeds silently reports
/// nothing), and so is "Could not set the block", which a `setblock` answers
/// when the block is already there; the caller confirms the effect from the
/// client side.
pub fn run_all(rcon: &mut RconClient, commands: &[String]) {
    for command in commands {
        let reply = rcon.cmd(command);
        let lower = reply.to_lowercase();
        assert!(
            !(lower.contains("unknown")
                || lower.contains("incorrect")
                || lower.contains("expected")
                || lower.contains("not loaded")
                || lower.contains("no entity")),
            "RCON `{command}` was refused: {reply:?}"
        );
    }
}

/// Polls `check` every 100 ms until it returns `Some` or `timeout` elapses.
pub fn wait_for<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = check() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The two probe signs the sign gates read, each on a stone block: the first
/// mixes styled and unstyled lines (the discriminating wire shape), the second
/// is all plain.
pub fn probe_sign_commands(mixed: [i32; 3], plain: [i32; 3]) -> Vec<String> {
    let [mx, my, mz] = mixed;
    let [px, py, pz] = plain;
    vec![
        format!("setblock {mx} {} {mz} minecraft:stone replace", my - 1),
        format!(
            "setblock {mx} {my} {mz} minecraft:oak_sign[rotation=0]{{front_text:{{messages:[{{text:\"REDLINE\",color:\"red\"}},{{text:\"BOLDY\",bold:1b}},\"plain\",\"\"]}}}} replace"
        ),
        format!("setblock {px} {} {pz} minecraft:stone replace", py - 1),
        format!(
            "setblock {px} {py} {pz} minecraft:oak_sign[rotation=0]{{front_text:{{messages:[\"allplain\",\"second\",\"\",\"\"]}}}} replace"
        ),
    ]
}
