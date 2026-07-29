//! Live proof that real vanilla-26.2 server chunks cross the **shell's own
//! public section-read seam** ([`NetClient::sections_at`]) as genuine terrain —
//! the honest core of the "live chunk → shell" milestone, asserted block-for-
//! block rather than screenshotted.
//!
//! This is `#[ignore]`d and gated on `--features live`, so running it is an
//! explicit opt-in. Per the project rule, a missing precondition is therefore a
//! **failure with a fix hint**, never a silent skip: if the server is down or
//! the client never logs in, the test panics and tells you how to repair the
//! environment, because a gate that passes with no server is worse than no gate.
//!
//! Anti-vacuity is built in three ways, so "correctly rendered nothing" cannot
//! pass:
//!   * an all-`None` result (blackout / wrong index range) fails,
//!   * a near-empty result (< 100 non-air blocks) fails, and
//!   * a single-state fill (< 2 distinct block states) fails.
//!
//! Real 26.2 terrain carries many states (bedrock, stone, dirt, grass, ores…).
#![cfg(feature = "live")]

use std::collections::HashSet;
use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
/// Vanilla 26.2. Named only as a protocol *number* — the shell never names a
/// version — and resolved through the registry by the `live` feature.
const PROTOCOL: i32 = 776;

#[test]
#[ignore = "requires the live lodestone-mc262 server on 127.0.0.1:25565 (`docker start lodestone-mc262`) and `--features live`"]
fn live_sections_cross_the_shell_seam_as_real_terrain() {
    let net = NetClient::connect(HOST.into(), PORT, PROTOCOL, None);

    let deadline = Instant::now() + Duration::from_secs(45);
    let mut logged_in = false;
    let mut chunk_signals = 0usize;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Chunk { .. } => chunk_signals += 1,
                NetUpdate::Error(e) => last_err = Some(e),
                NetUpdate::Disconnected(r) => last_err = Some(format!("disconnected: {r}")),
                _ => {}
            }
        }
        if logged_in && net.loaded_chunks().len() >= 4 {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    assert!(
        logged_in,
        "never logged in to {HOST}:{PORT} within 45s (last event: {last_err:?}). \
         Fix: `docker start lodestone-mc262` and run with `--features live`."
    );

    let loaded = net.loaded_chunks();
    assert!(
        !loaded.is_empty(),
        "logged in but the client holds zero columns ({chunk_signals} chunk signals seen). \
         A dead shared offline player causes a total chunk blackout; this run used a unique \
         username, so a healthy server should stream chunks — check the server."
    );

    // Pull sections across the loaded columns through the *shell's* public seam.
    // Indices 0..24 span an overworld column; out-of-range indices return `None`
    // (the column-geometry seam that would bound this precisely is not exposed
    // yet), which is harmless for a data proof.
    let mut requests = Vec::new();
    for pos in loaded.iter().take(16) {
        for si in 0..24usize {
            requests.push((*pos, si));
        }
    }
    let sections = net.sections_at(&requests);

    let present = sections.iter().filter(|s| s.is_some()).count();
    let mut total_non_air: u64 = 0;
    let mut distinct: HashSet<u32> = HashSet::new();
    for section in sections.iter().flatten() {
        total_non_air += u64::from(section.non_air_count());
        let air = section.air_id();
        for y in 0..16 {
            for z in 0..16 {
                for x in 0..16 {
                    let id = section.get_block(x, y, z);
                    if id != air {
                        distinct.insert(id);
                    }
                }
            }
        }
    }

    assert!(
        present > 0,
        "sections_at returned only `None` across {} columns — the seam handed back no \
         resident sections (blackout or wrong index range).",
        loaded.len()
    );
    assert!(
        total_non_air > 100,
        "only {total_non_air} non-air blocks across {present} live sections — too sparse \
         to be real terrain (a blackout masquerading as success)."
    );
    assert!(
        distinct.len() >= 2,
        "live terrain shows only {} distinct block state(s) — a single-state fill is not \
         real world data.",
        distinct.len()
    );

    eprintln!(
        "live section seam OK: {} columns, {present} resident sections, {total_non_air} \
         non-air blocks, {} distinct block states",
        loaded.len(),
        distinct.len()
    );
}
