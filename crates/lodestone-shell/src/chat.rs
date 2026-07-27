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

/// How many chat lines the scrollback retains. Older lines are dropped.
const MAX_LINES: usize = 100;

/// A bounded, newest-last scrollback of received chat/system lines (already
/// flattened to plain text by the caller).
#[derive(Debug, Clone, Default)]
pub struct ChatLog {
    lines: VecDeque<String>,
}

impl ChatLog {
    /// A fresh, empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a received line, evicting the oldest if at capacity.
    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push_back(line.into());
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    /// The most recent `n` lines, oldest-first (render order, top to bottom).
    #[must_use]
    pub fn recent(&self, n: usize) -> Vec<&str> {
        let start = self.lines.len().saturating_sub(n);
        self.lines.iter().skip(start).map(String::as_str).collect()
    }

    /// Total retained lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
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

    #[test]
    fn log_keeps_newest_and_bounds_length() {
        let mut log = ChatLog::new();
        for i in 0..(MAX_LINES + 10) {
            log.push(format!("line {i}"));
        }
        assert_eq!(log.len(), MAX_LINES, "log must evict oldest at capacity");
        let recent = log.recent(3);
        // The three newest survive, oldest-first.
        assert_eq!(
            recent,
            [
                format!("line {}", MAX_LINES + 7),
                format!("line {}", MAX_LINES + 8),
                format!("line {}", MAX_LINES + 9),
            ]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn recent_handles_asking_for_more_than_exist() {
        let mut log = ChatLog::new();
        log.push("only");
        assert_eq!(log.recent(10), vec!["only"]);
        assert!(ChatLog::new().recent(5).is_empty());
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
