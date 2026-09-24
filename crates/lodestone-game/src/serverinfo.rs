//! Server-announced session metadata: links, report details, chat completions,
//! tick rate and dialogs.
//!
//! ## What it is
//!
//! One store for the five things a server tells the client about *itself* rather
//! than about the world: its advertised links, its crash-report metadata, the
//! extra names it wants offered in chat completion, its tick rate / freeze state,
//! and whichever dialog it currently wants open.
//!
//! They share a store because they share a lifetime and a consumer shape — all
//! five are read by chrome (the pause screen, the chat box, the debug overlay),
//! none is per-entity, and each is a handful of fields. Five separate components
//! would be five folds and five registrations for no separation anyone benefits
//! from.
//!
//! ## How it works
//!
//! * **Links and report details** replace wholesale: each packet carries the
//!   complete set.
//! * **Chat completions** are a three-way action — add, remove, or replace — and
//!   the *replace* case is why this cannot be a plain `extend`.
//! * **Ticking state** is `(rate, frozen)`. `ticking_step` is a *separate* packet
//!   and does not imply frozen; it says how many ticks remain to run while frozen,
//!   so it is stored beside the flag rather than overwriting it.
//! * **Dialogs** are a single slot: `show_dialog` sets it, `clear_dialog` empties
//!   it. A dialog is either a registry reference or an inline NBT blob and this
//!   store keeps whichever arrived — parsing the blob is a renderer's job (see
//!   [`lodestone_model::event::ClientEvent::DialogShown`] for why).
//!
//! ## How to change it
//!
//! Everything here is server-authored and **untrusted**: a link URL is an
//! arbitrary string and a custom link label an arbitrary component. Nothing in
//! this crate validates either, and a consumer that opens a URL must be the thing
//! that asks the player first.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::ClientEvent`] only.

use std::collections::BTreeSet;

use lodestone_model::event::{ChatCompletionsAction, ClientEvent, ServerLink};

/// The dialog the server currently wants open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenDialog {
    /// A dialog from the `minecraft:dialog` registry, by network id.
    Registry(i32),
    /// An inline dialog as raw network-NBT bytes.
    Inline(Vec<u8>),
}

/// The server's tick pacing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickingState {
    /// Ticks per second the server targets. Vanilla's default is 20.
    pub tick_rate: f32,
    /// Whether the world is frozen.
    pub frozen: bool,
    /// Ticks remaining to run while frozen, from the last `ticking_step`.
    pub pending_steps: i32,
}

impl Default for TickingState {
    /// Vanilla's own default, which is a real state and not a placeholder: a
    /// server that never sends `ticking_state` is running 20 t/s unfrozen.
    fn default() -> Self {
        Self {
            tick_rate: 20.0,
            frozen: false,
            pending_steps: 0,
        }
    }
}

/// Everything the server has announced about itself this session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerInfoStore {
    links: Vec<ServerLink>,
    report_details: Vec<(String, String)>,
    chat_completions: BTreeSet<String>,
    ticking: TickingState,
    dialog: Option<OpenDialog>,
    low_disk_space_warnings: u32,
}

impl ServerInfoStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The server's advertised links, in the order sent. **Untrusted** — see the
    /// module doc.
    #[must_use]
    pub fn links(&self) -> &[ServerLink] {
        &self.links
    }

    /// The server's crash-report metadata, `(title, description)`.
    #[must_use]
    pub fn report_details(&self) -> &[(String, String)] {
        &self.report_details
    }

    /// The extra names to offer in chat completion, sorted.
    #[must_use]
    pub fn chat_completions(&self) -> impl Iterator<Item = &String> {
        self.chat_completions.iter()
    }

    /// The server's tick pacing.
    #[must_use]
    pub fn ticking(&self) -> TickingState {
        self.ticking
    }

    /// The dialog the server wants open, if any.
    #[must_use]
    pub fn dialog(&self) -> Option<&OpenDialog> {
        self.dialog.as_ref()
    }

    /// How many low-disk-space warnings the server has sent.
    ///
    /// A count rather than a boolean so a consumer can tell one warning from a
    /// server that is warning repeatedly, and so a test has a fact about
    /// traversal to assert — the packet carries no payload at all, and a
    /// `bool` set to `true` would be satisfied by a `Default` that guessed.
    #[must_use]
    pub fn low_disk_space_warnings(&self) -> u32 {
        self.low_disk_space_warnings
    }

    /// Folds one event, returning whether it belonged to this store.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::ServerLinksReceived { links } => {
                self.links = links.clone();
                true
            }
            ClientEvent::CustomReportDetails { details } => {
                self.report_details = details.clone();
                true
            }
            ClientEvent::ChatCompletionsChanged { action, entries } => {
                match action {
                    ChatCompletionsAction::Add => {
                        self.chat_completions.extend(entries.iter().cloned());
                    }
                    ChatCompletionsAction::Remove => {
                        for entry in entries {
                            self.chat_completions.remove(entry);
                        }
                    }
                    // The case a plain `extend` gets wrong.
                    ChatCompletionsAction::Set => {
                        self.chat_completions = entries.iter().cloned().collect();
                    }
                }
                true
            }
            ClientEvent::TickingStateChanged { tick_rate, frozen } => {
                self.ticking.tick_rate = *tick_rate;
                self.ticking.frozen = *frozen;
                true
            }
            ClientEvent::TickingStepped { tick_steps } => {
                // Deliberately does not touch `frozen`: stepping is a separate
                // packet and the server sends `ticking_state` for the flag.
                self.ticking.pending_steps = *tick_steps;
                true
            }
            ClientEvent::DialogShown {
                registry_id,
                inline,
            } => {
                self.dialog = match (registry_id, inline) {
                    (Some(id), _) => Some(OpenDialog::Registry(*id)),
                    (None, Some(bytes)) => Some(OpenDialog::Inline(bytes.clone())),
                    // Neither half present cannot happen off the wire, but
                    // clearing is the safe reading of it.
                    (None, None) => None,
                };
                true
            }
            ClientEvent::DialogCleared => {
                self.dialog = None;
                true
            }
            ClientEvent::LowDiskSpaceWarning => {
                self.low_disk_space_warnings = self.low_disk_space_warnings.saturating_add(1);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenDialog, ServerInfoStore};
    use lodestone_model::event::{ChatCompletionsAction, ClientEvent};

    /// `Set` must replace, not merge. This is the arm a plain `extend` breaks.
    #[test]
    fn set_replaces_the_completion_set_rather_than_merging_into_it() {
        let mut store = ServerInfoStore::new();
        store.apply(&ClientEvent::ChatCompletionsChanged {
            action: ChatCompletionsAction::Add,
            entries: vec!["alice".to_owned(), "bob".to_owned()],
        });
        assert_eq!(store.chat_completions().count(), 2);

        store.apply(&ClientEvent::ChatCompletionsChanged {
            action: ChatCompletionsAction::Set,
            entries: vec!["carol".to_owned()],
        });
        assert_eq!(
            store.chat_completions().cloned().collect::<Vec<_>>(),
            vec!["carol".to_owned()],
            "Set must discard the previous set"
        );

        store.apply(&ClientEvent::ChatCompletionsChanged {
            action: ChatCompletionsAction::Remove,
            entries: vec!["carol".to_owned()],
        });
        assert_eq!(store.chat_completions().count(), 0);
    }

    /// The default is vanilla's real state, not a sentinel.
    #[test]
    fn the_default_ticking_state_is_twenty_unfrozen() {
        let store = ServerInfoStore::new();
        assert_eq!(store.ticking().tick_rate, 20.0);
        assert!(!store.ticking().frozen);
    }

    /// Stepping must not silently assert frozen — the flag has its own packet.
    #[test]
    fn a_step_does_not_change_the_frozen_flag() {
        let mut store = ServerInfoStore::new();
        store.apply(&ClientEvent::TickingStateChanged {
            tick_rate: 5.0,
            frozen: true,
        });
        store.apply(&ClientEvent::TickingStepped { tick_steps: 3 });
        assert!(store.ticking().frozen);
        assert_eq!(store.ticking().pending_steps, 3);
        assert_eq!(store.ticking().tick_rate, 5.0);
    }

    #[test]
    fn a_dialog_is_one_slot_and_clear_empties_it() {
        let mut store = ServerInfoStore::new();
        store.apply(&ClientEvent::DialogShown {
            registry_id: Some(4),
            inline: None,
        });
        assert_eq!(store.dialog(), Some(&OpenDialog::Registry(4)));
        store.apply(&ClientEvent::DialogShown {
            registry_id: None,
            inline: Some(vec![0x0A, 0x00]),
        });
        assert_eq!(store.dialog(), Some(&OpenDialog::Inline(vec![0x0A, 0x00])));
        store.apply(&ClientEvent::DialogCleared);
        assert!(store.dialog().is_none());
    }

    #[test]
    fn low_disk_space_is_counted_so_a_default_cannot_fake_it() {
        let mut store = ServerInfoStore::new();
        assert_eq!(store.low_disk_space_warnings(), 0);
        store.apply(&ClientEvent::LowDiskSpaceWarning);
        store.apply(&ClientEvent::LowDiskSpaceWarning);
        assert_eq!(store.low_disk_space_warnings(), 2);
    }

    #[test]
    fn an_unrelated_event_is_rejected() {
        let mut store = ServerInfoStore::new();
        assert!(!store.apply(&ClientEvent::KeepAlive { id: 1 }));
    }
}
