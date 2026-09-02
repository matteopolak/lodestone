//! libFuzzer target: `lodestone_model::text::Text::from_json` must never
//! panic on arbitrary (but UTF-8) bytes.
//!
//! Issue #549's "obvious first targets" list names "text/JSON parsing in
//! `lodestone-model`" — this is it. Pre-1.20.3 protocol families (`v1-8`,
//! `v1-9`, `v1-14`) send every chat message, sign line, book page and
//! scoreboard entry as a JSON-encoded text component, decoded by this one
//! function; `from_json`'s own doc already claims "never panics and is
//! depth-limited against hostile input" (`MAX_DEPTH = 64`), which is exactly
//! the kind of self-reported property this harness exists to turn into a
//! measurement instead of trusting the comment. A hostile server choosing
//! the chat message it sends a joining client is a real, low-effort attack
//! surface distinct from the wire-frame decoders the other targets cover —
//! this one is downstream of `handle_packet` succeeding, not a replacement
//! for fuzzing it.
//!
//! `from_json` degrades on a parse failure (returns the raw input as a
//! literal) rather than returning `Result`, so there is no `Err` arm to
//! branch on here — the only property this target can check is "did not
//! panic and did not recurse without bound", matching
//! `no_panic_v26_2_serverbound.rs`'s note that some decode APIs give up a
//! `Result` split by design.

#![no_main]

use libfuzzer_sys::fuzz_target;
use lodestone_model::text::Text;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let _ = Text::from_json(text);
});
