//! Chat: an inbound scrollback log and an outbound input line.
//!
//! This is deliberately pure — no winit, no GPU, no client handle — so the
//! interesting behaviour (what a line of typed text *means*, how the log
//! bounds itself, how editing behaves at char boundaries) is unit-testable
//! without a window or a server. The platform layer ([`crate::app`]) feeds it
//! keystrokes and drains [`compose_chat_action`] onto the outbound
//! [`ClientAction`] seam; the HUD ([`crate::hud`]) reads the received scrollback
//! and the in-progress [`ChatInput`] to draw them.
//!
//! The received log itself is **not here**. `docs/bevy-migration.md` Stage 5
//! moved it to `lodestone_game::chat::ChatLog` so it could be the payload of a
//! `lodestone_ecs::SessionChat` component — `lodestone-ecs` cannot depend on this
//! crate. What is left here is the outbound half only.
//!
//! Routing a chat line to the wire goes through the *same* `ClientAction` seam
//! as movement, not a bespoke path: a leading `/` is a command, everything else
//! is a chat message, matching vanilla. The shell never names a packet.

use lodestone_assets::ResourceLocation;
use lodestone_client::ClientAction;

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

/// Outcome of running a typed line through [`intercept_give_debug`].
#[derive(Debug, Clone, PartialEq)]
pub enum GiveDebugOutcome {
    /// Not a `/givedebug` line; fall through to [`compose_chat_action`].
    NotGiveDebug,
    /// A well-formed `/givedebug <item> <amount>`, translated to the real
    /// command. `local_echo` is a chat-log line showing the translation;
    /// `action` is what should actually be sent to the server.
    Send {
        /// What to show locally so the user can see what was actually sent.
        local_echo: String,
        /// The real `/give @s <item> <amount>` command action.
        action: ClientAction,
    },
    /// `/givedebug` was typed but malformed. `message` is for local display
    /// only — nothing is sent to the server. A debug command that fails
    /// quietly is worse than no command at all.
    Error(String),
}

/// Intercepts the testing-only `/givedebug <item> <amount>` command, deliberately
/// distinct from vanilla's `/give`: no NBT, no components, no selectors, no tab
/// completion. `item` is a plain namespaced id (e.g. `minecraft:diamond_pickaxe`,
/// with the `minecraft:` namespace implied if omitted — see
/// [`ResourceLocation::parse`]) and `amount` is a positive integer count.
///
/// We are a client, not the inventory's authority: this never mutates local
/// inventory state. It composes the server's own `/give @s <item> <amount>` and
/// hands it back as a normal [`ClientAction::SendCommand`] for the caller to
/// send exactly like any other typed command — the server remains the one
/// source of truth, so there is nothing to desync. It needs the player to be
/// **op** on the server; if the server refuses, that refusal comes back as an
/// ordinary chat message over the existing inbound path, not from here.
///
/// Called ahead of [`compose_chat_action`], not as a replacement for it:
/// anything that is not `/givedebug` returns [`GiveDebugOutcome::NotGiveDebug`]
/// so the normal command/chat routing is untouched.
#[must_use]
pub fn intercept_give_debug(line: &str) -> GiveDebugOutcome {
    let Some(rest) = line.trim_end().strip_prefix('/') else {
        return GiveDebugOutcome::NotGiveDebug;
    };
    let mut words = rest.split_whitespace();
    match words.next() {
        Some("givedebug") => {}
        _ => return GiveDebugOutcome::NotGiveDebug,
    }

    let usage = "givedebug: usage /givedebug <item> <amount>, e.g. /givedebug minecraft:diamond_pickaxe 1";

    let Some(item) = words.next() else {
        return GiveDebugOutcome::Error(usage.to_string());
    };
    let Some(amount_str) = words.next() else {
        return GiveDebugOutcome::Error(usage.to_string());
    };
    if words.next().is_some() {
        return GiveDebugOutcome::Error(format!("{usage} (too many arguments)"));
    }

    let location = match ResourceLocation::parse(item) {
        Ok(location) => location,
        Err(_) => {
            return GiveDebugOutcome::Error(format!(
                "givedebug: '{item}' is not a valid item id, e.g. minecraft:diamond_pickaxe"
            ));
        }
    };
    let amount: u32 = match amount_str.parse() {
        Ok(amount) if amount >= 1 => amount,
        _ => {
            return GiveDebugOutcome::Error(format!(
                "givedebug: '{amount_str}' is not a valid positive amount"
            ));
        }
    };

    let command = format!("give @s {location} {amount}");
    GiveDebugOutcome::Send {
        local_echo: format!("givedebug: sending /{command}"),
        action: ClientAction::SendCommand { command },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn givedebug_translates_to_the_real_give_command() {
        match intercept_give_debug("/givedebug minecraft:diamond_pickaxe 1") {
            GiveDebugOutcome::Send { local_echo, action } => {
                assert_eq!(
                    action,
                    ClientAction::SendCommand {
                        command: "give @s minecraft:diamond_pickaxe 1".to_string(),
                    }
                );
                assert!(
                    local_echo.contains("give @s minecraft:diamond_pickaxe 1"),
                    "echo must show the actual translation, got {local_echo:?}"
                );
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn givedebug_defaults_the_namespace_like_vanilla() {
        match intercept_give_debug("/givedebug diamond_pickaxe 1") {
            GiveDebugOutcome::Send { action, .. } => {
                assert_eq!(
                    action,
                    ClientAction::SendCommand {
                        command: "give @s minecraft:diamond_pickaxe 1".to_string(),
                    },
                    "a bare item id must default to the minecraft: namespace"
                );
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn givedebug_rejects_a_malformed_item_id_locally() {
        match intercept_give_debug("/givedebug not a valid id!! 1") {
            GiveDebugOutcome::Error(_) => {}
            other => panic!("expected a local Error, got {other:?}"),
        }
    }

    #[test]
    fn givedebug_rejects_a_non_numeric_or_zero_amount_locally() {
        for line in [
            "/givedebug minecraft:diamond_pickaxe abc",
            "/givedebug minecraft:diamond_pickaxe 0",
            "/givedebug minecraft:diamond_pickaxe -1",
        ] {
            match intercept_give_debug(line) {
                GiveDebugOutcome::Error(_) => {}
                other => panic!("expected a local Error for {line:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn givedebug_rejects_wrong_argument_counts_locally() {
        for line in [
            "/givedebug",
            "/givedebug minecraft:diamond_pickaxe",
            "/givedebug minecraft:diamond_pickaxe 1 extra",
        ] {
            match intercept_give_debug(line) {
                GiveDebugOutcome::Error(_) => {}
                other => panic!("expected a local Error for {line:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn non_givedebug_lines_fall_through_untouched() {
        for line in ["/gamemode creative", "plain message", "", "/givedebugger 1 2"] {
            assert_eq!(
                intercept_give_debug(line),
                GiveDebugOutcome::NotGiveDebug,
                "must not intercept {line:?}"
            );
        }
    }
}
