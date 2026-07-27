//! Chat: an inbound scrollback log and an outbound input line.
//!
//! This is deliberately pure — no winit, no GPU, no client handle — so the
//! interesting behaviour (what a line of typed text *means*, how the log
//! bounds itself, how editing behaves at char boundaries) is unit-testable
//! without a window or a server. The platform layer ([`crate::app`]) feeds it
//! keystrokes and drains [`compose_chat_action`] onto the outbound
//! [`ClientAction`] seam; the HUD ([`crate::hud`]) reads [`ChatLog::recent`] and
//! the in-progress [`ChatInput`] to draw them.
//!
//! Routing a chat line to the wire goes through the *same* `ClientAction` seam
//! as movement, not a bespoke path: a leading `/` is a command, everything else
//! is a chat message, matching vanilla. The shell never names a packet.

use std::collections::VecDeque;

use lodestone_client::ClientAction;
use lodestone_game::chat::{ChatEntry, ChatFeed, MessageTrust};
use lodestone_model::Text;

/// The display component of a feed entry, regardless of variant.
fn entry_display(entry: &ChatEntry) -> &Text {
    match entry {
        ChatEntry::Player { display, .. } => display,
        ChatEntry::System { content } => content,
    }
}

/// The received chat scrollback.
///
/// The message *content* model — bounding, ordering, trust, the 100-line cap —
/// is [`lodestone_game::chat::ChatFeed`], the version-free canonical feed the
/// game crate owns; the shell does not reimplement it. What the shell adds here
/// is purely a **render concern**: the monotonic arrival time of each entry,
/// which drives the vanilla fade-out (a client-renderer detail that vanilla
/// itself keeps in `ChatComponent`, not in server state). The two structures
/// are pushed and evicted in lockstep so index *i* of one matches the other.
///
/// Times are plain `f64` seconds supplied by the caller, so this module stays
/// free of any clock (and thus wasm-safe and unit-testable without a real time
/// source).
#[derive(Debug, Clone, Default)]
pub struct ChatLog {
    feed: ChatFeed,
    times: VecDeque<f64>,
}

impl ChatLog {
    /// A fresh, empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the entry's arrival time, evicting the oldest in lockstep with the
    /// feed so the two stay index-aligned.
    fn stamp(&mut self, at: f64) {
        if self.times.len() == self.feed.capacity() {
            self.times.pop_front();
        }
        self.times.push_back(at);
    }

    /// Append a decorated player/disguised message (its `display` component is
    /// already the server-decorated `<sender> body`), stamped with the caller's
    /// monotonic clock (`at`, in seconds).
    pub fn push_player(&mut self, display: Text, trust: MessageTrust, at: f64) {
        self.feed.push_player(display, trust);
        self.stamp(at);
    }

    /// Append a system message, stamped with the caller's monotonic clock.
    pub fn push_system(&mut self, content: Text, at: f64) {
        self.feed.push_system(content);
        self.stamp(at);
    }

    /// The most recent `n` lines, oldest-first (render order, top to bottom),
    /// each flattened to a legacy `§`-code string at read time (colour survives
    /// once the adapter preserves it) and paired with its arrival timestamp.
    #[must_use]
    pub fn recent(&self, n: usize) -> Vec<(String, f64)> {
        let start = self.feed.len().saturating_sub(n);
        self.feed
            .iter()
            .zip(self.times.iter())
            .skip(start)
            .map(|(entry, at)| (entry_display(entry).to_legacy_string(), *at))
            .collect()
    }

    /// Total retained lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.feed.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.feed.is_empty()
    }
}

/// The line currently being typed. Kept separate from the log so opening the
/// chat box, editing, and cancelling never touch the received history.
#[derive(Debug, Clone, Default)]
pub struct ChatInput {
    buf: String,
}

impl ChatInput {
    /// An empty input.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the buffer (e.g. a leading `/` when chat is opened with the command
    /// key). Replaces any current contents.
    pub fn set(&mut self, text: impl Into<String>) {
        self.buf = text.into();
    }

    /// Append typed text. Control characters (newlines, the section sign used by
    /// legacy colour codes) are filtered so a paste or an IME can't inject them.
    pub fn push_str(&mut self, text: &str) {
        for ch in text.chars() {
            self.push_char(ch);
        }
    }

    /// Append a single character if it is printable and the line has room.
    /// Vanilla caps a chat line at 256 characters.
    pub fn push_char(&mut self, ch: char) {
        if ch.is_control() || ch == '\u{00a7}' {
            return;
        }
        if self.buf.chars().count() >= 256 {
            return;
        }
        self.buf.push(ch);
    }

    /// Delete the last character (char-boundary safe). No-op when empty.
    pub fn backspace(&mut self) {
        self.buf.pop();
    }

    /// The current text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// Whether nothing has been typed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Clear and return the typed line, ready to compose into an action.
    #[must_use]
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.buf)
    }
}

/// Lower a typed line onto the outbound action seam, matching vanilla's rule:
/// a leading `/` is a command (sent without the slash), anything else is chat.
/// A blank line — or a bare `/` — sends nothing.
#[must_use]
pub fn compose_chat_action(line: &str) -> Option<ClientAction> {
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    if let Some(command) = line.strip_prefix('/') {
        if command.is_empty() {
            return None;
        }
        Some(ClientAction::SendCommand {
            command: command.to_string(),
        })
    } else {
        Some(ClientAction::SendChat {
            text: line.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors [`lodestone_game::chat::ChatFeed`]'s default capacity.
    const MAX_LINES: usize = 100;

    #[test]
    fn log_keeps_newest_and_bounds_length() {
        let mut log = ChatLog::new();
        for i in 0..(MAX_LINES + 10) {
            log.push_system(Text::literal(format!("line {i}")), i as f64);
        }
        assert_eq!(log.len(), MAX_LINES, "log must evict oldest at capacity");
        let recent: Vec<String> = log.recent(3).into_iter().map(|(line, _)| line).collect();
        // The three newest survive, oldest-first.
        assert_eq!(
            recent,
            [
                format!("line {}", MAX_LINES + 7),
                format!("line {}", MAX_LINES + 8),
                format!("line {}", MAX_LINES + 9),
            ]
        );
    }

    #[test]
    fn recent_handles_asking_for_more_than_exist() {
        let mut log = ChatLog::new();
        log.push_system(Text::literal("only"), 0.0);
        assert_eq!(
            log.recent(10)
                .into_iter()
                .map(|(l, _)| l)
                .collect::<Vec<_>>(),
            vec!["only".to_string()]
        );
        assert!(ChatLog::new().recent(5).is_empty());
    }

    #[test]
    fn recent_carries_arrival_timestamps() {
        let mut log = ChatLog::new();
        log.push_system(Text::literal("first"), 1.5);
        log.push_system(Text::literal("second"), 4.25);
        assert_eq!(
            log.recent(2),
            vec![("first".to_string(), 1.5), ("second".to_string(), 4.25)]
        );
    }

    #[test]
    fn input_edits_are_char_boundary_safe() {
        let mut input = ChatInput::new();
        input.push_str("héllo"); // multi-byte é
        input.backspace(); // removes 'o'
        input.backspace(); // removes 'l'
        assert_eq!(input.as_str(), "hél");
        input.backspace();
        input.backspace(); // removes é (2 bytes) as one char
        assert_eq!(input.as_str(), "h");
    }

    #[test]
    fn input_filters_control_chars_and_section_sign() {
        let mut input = ChatInput::new();
        input.push_str("hi\nthere\u{00a7}c");
        assert_eq!(input.as_str(), "hitherec");
    }

    #[test]
    fn input_caps_at_256_chars() {
        let mut input = ChatInput::new();
        input.push_str(&"a".repeat(300));
        assert_eq!(input.as_str().chars().count(), 256);
    }

    #[test]
    fn take_clears_and_returns() {
        let mut input = ChatInput::new();
        input.set("hello");
        assert_eq!(input.take(), "hello");
        assert!(input.is_empty());
    }

    #[test]
    fn plain_text_composes_a_chat_message() {
        match compose_chat_action("hello world") {
            Some(ClientAction::SendChat { text }) => assert_eq!(text, "hello world"),
            other => panic!("expected SendChat, got {other:?}"),
        }
    }

    #[test]
    fn slash_composes_a_command_without_the_slash() {
        match compose_chat_action("/gamemode creative") {
            Some(ClientAction::SendCommand { command }) => {
                assert_eq!(command, "gamemode creative", "slash must be stripped");
            }
            other => panic!("expected SendCommand, got {other:?}"),
        }
    }

    #[test]
    fn blank_and_bare_slash_send_nothing() {
        assert!(compose_chat_action("").is_none());
        assert!(compose_chat_action("   ").is_none());
        assert!(compose_chat_action("/").is_none());
    }
}
