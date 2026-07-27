//! Version-free chat state: message decoration, trust, filtering, and the
//! rolling chat feed.
//!
//! ## What lives here vs. what a version adapter owns
//!
//! This crate never sees packet bytes or cryptography. A version adapter decodes
//! `player_chat` / `system_chat` / `disguised_chat` into the canonical shapes
//! here ([`PlayerChatMessage`], [`SystemMessage`], [`DisguisedChatMessage`]) and,
//! crucially, performs **signature verification** with the sender's public key,
//! reporting only the *result* as a [`MessageTrust`]. Ed25519/RSA verification
//! and the signed-message chain (previous-message links, salts, session keys)
//! are inherently version- and crypto-specific and deliberately stay out of the
//! version-free layer; what stays here is the trust *state* and how a message
//! renders once trust is known.
//!
//! ## Decoration
//!
//! A player/disguised message is rendered by applying a [`ChatDecoration`] (the
//! bound chat type's translation key + parameter order + style) to the message
//! content, substituting the sender name, target name, and body — exactly the
//! vanilla `ChatType.Bound.decorate` operation, expressed against
//! [`lodestone_model::Text`].

use std::collections::VecDeque;

use lodestone_model::{Text, TextStyle};
use uuid::Uuid;

/// A parameter slot substituted into a chat-type decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatParameter {
    /// The sender's display name.
    Sender,
    /// The target's display name (direct/whisper messages).
    Target,
    /// The message body.
    Content,
}

/// A chat-type decoration: a translation key, the ordered parameters it takes,
/// and a style applied to the resulting component.
///
/// Vanilla's `minecraft:chat` is [`ChatDecoration::vanilla_chat`] — key
/// `chat.type.text` with `[Sender, Content]`, rendering `<Sender> body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDecoration {
    /// The translation key (e.g. `chat.type.text`).
    pub translation_key: String,
    /// The parameters, in substitution order.
    pub parameters: Vec<ChatParameter>,
    /// The style applied to the decorated component.
    pub style: TextStyle,
}

impl ChatDecoration {
    /// Builds a decoration.
    #[must_use]
    pub fn new(
        translation_key: impl Into<String>,
        parameters: Vec<ChatParameter>,
        style: TextStyle,
    ) -> Self {
        Self {
            translation_key: translation_key.into(),
            parameters,
            style,
        }
    }

    /// The vanilla `minecraft:chat` decoration: `chat.type.text` with
    /// `[Sender, Content]`.
    #[must_use]
    pub fn vanilla_chat() -> Self {
        Self::new(
            "chat.type.text",
            vec![ChatParameter::Sender, ChatParameter::Content],
            TextStyle::default(),
        )
    }

    /// Decorates `content` into a display component by substituting the sender
    /// name, target name, and body into this decoration's translation.
    ///
    /// A `Target` parameter with no `target_name` resolves to empty text,
    /// matching vanilla.
    #[must_use]
    pub fn decorate(&self, content: Text, sender_name: &Text, target_name: Option<&Text>) -> Text {
        let with: Vec<Text> = self
            .parameters
            .iter()
            .map(|param| match param {
                ChatParameter::Sender => sender_name.clone(),
                ChatParameter::Target => target_name.cloned().unwrap_or_default(),
                ChatParameter::Content => content.clone(),
            })
            .collect();
        let mut decorated = Text::translate(self.translation_key.clone(), with);
        decorated.style = self.style;
        decorated
    }
}

/// How much the client trusts a signed player message.
///
/// A version adapter that holds the sender's public key performs the actual
/// signature check and reports the outcome; this crate only carries the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTrust {
    /// Signature present and valid, content unmodified.
    Secure,
    /// No signature, or the sender has no key — unverifiable.
    NotSecure,
    /// Signed, but the shown content differs from what was signed.
    Modified,
}

impl MessageTrust {
    /// Whether the message is anything other than [`MessageTrust::Secure`].
    #[must_use]
    pub fn is_not_secure(self) -> bool {
        !matches!(self, MessageTrust::Secure)
    }
}

/// The server's content filter verdict for a message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FilterMask {
    /// Nothing filtered; show the content as-is.
    #[default]
    PassThrough,
    /// The whole message is filtered; do not show it.
    FullyFiltered,
    /// Per-character mask; `true` means that character is filtered.
    Partial(Vec<bool>),
}

impl FilterMask {
    /// Applies the mask to a raw message body, replacing filtered characters
    /// with `#`. Returns `None` when the message is fully filtered (hidden).
    #[must_use]
    pub fn apply(&self, text: &str) -> Option<String> {
        match self {
            FilterMask::PassThrough => Some(text.to_string()),
            FilterMask::FullyFiltered => None,
            FilterMask::Partial(mask) => Some(
                text.chars()
                    .enumerate()
                    .map(|(i, c)| {
                        if mask.get(i).copied().unwrap_or(false) {
                            '#'
                        } else {
                            c
                        }
                    })
                    .collect(),
            ),
        }
    }

    /// Whether the mask hides or alters any content.
    #[must_use]
    pub fn is_pass_through(&self) -> bool {
        matches!(self, FilterMask::PassThrough)
    }
}

/// A signed player chat message in canonical, version-free form.
///
/// The wire packet's cryptographic fields (`salt`, `signature`, previous-message
/// links) are carried as opaque data for a version adapter to verify; this layer
/// treats them as evidence, not as something it validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerChatMessage {
    /// The sender's player UUID.
    pub sender: Uuid,
    /// The per-sender message index (chain ordering).
    pub index: i32,
    /// The plaintext body that was (or would be) signed.
    pub signed_content: String,
    /// Server-supplied replacement content, if the server decorated it.
    pub unsigned_content: Option<Text>,
    /// Message timestamp, epoch milliseconds.
    pub timestamp_ms: i64,
    /// The signature salt (opaque; for the adapter's verification).
    pub salt: i64,
    /// The message signature bytes, if signed (opaque; adapter-verified).
    pub signature: Option<Vec<u8>>,
    /// The server's filter verdict.
    pub filter_mask: FilterMask,
    /// The sender's resolved display name.
    pub sender_name: Text,
    /// The target's display name, for direct messages.
    pub target_name: Option<Text>,
}

impl PlayerChatMessage {
    /// The content to decorate: the server's `unsigned_content` if present, else
    /// the signed plaintext as a literal (mirrors vanilla `decoratedContent`).
    #[must_use]
    pub fn decorated_content(&self) -> Text {
        self.unsigned_content
            .clone()
            .unwrap_or_else(|| Text::literal(self.signed_content.clone()))
    }

    /// Whether the message carries a signature.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        self.signature.is_some()
    }

    /// The final display component: the decoration applied to the (optionally
    /// filtered) content. Returns `None` when fully filtered.
    ///
    /// When `unsigned_content` is present it is shown verbatim (already the
    /// server's chosen rendering); otherwise the signed plaintext is passed
    /// through the filter mask before decoration.
    #[must_use]
    pub fn display(&self, decoration: &ChatDecoration) -> Option<Text> {
        let content = match &self.unsigned_content {
            Some(text) => text.clone(),
            None => match self.filter_mask.apply(&self.signed_content) {
                Some(filtered) => Text::literal(filtered),
                None => return None,
            },
        };
        Some(decoration.decorate(content, &self.sender_name, self.target_name.as_ref()))
    }
}

/// A system message. `overlay` true means it renders on the action bar rather
/// than in the chat feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemMessage {
    /// The message content (already a full component).
    pub content: Text,
    /// Whether to show it on the action bar (`true`) or in chat (`false`).
    pub overlay: bool,
}

/// A disguised chat message: server-decorated like player chat, but carries no
/// signature and is therefore never cryptographically trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisguisedChatMessage {
    /// The message content to decorate.
    pub content: Text,
    /// The sender's display name.
    pub sender_name: Text,
    /// The target's display name, if any.
    pub target_name: Option<Text>,
}

impl DisguisedChatMessage {
    /// The final display component (decoration applied). Always
    /// [`MessageTrust::NotSecure`] when logged, since it is unsigned.
    #[must_use]
    pub fn display(&self, decoration: &ChatDecoration) -> Text {
        decoration.decorate(
            self.content.clone(),
            &self.sender_name,
            self.target_name.as_ref(),
        )
    }
}

/// One rendered entry in the chat feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEntry {
    /// A player (or disguised) message, with its trust level.
    Player {
        /// The decorated display component.
        display: Text,
        /// The trust level to badge the message with.
        trust: MessageTrust,
    },
    /// A system message shown in the feed.
    System {
        /// The display component.
        content: Text,
    },
}

/// Vanilla keeps this many rendered chat lines by default.
const DEFAULT_CHAT_CAPACITY: usize = 100;

/// A bounded, most-recent-last chat feed. Oldest entries are dropped once the
/// capacity is exceeded.
#[derive(Debug, Clone)]
pub struct ChatFeed {
    entries: VecDeque<ChatEntry>,
    capacity: usize,
}

impl Default for ChatFeed {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CHAT_CAPACITY)
    }
}

impl ChatFeed {
    /// A feed with the default capacity (100).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A feed holding at most `capacity` entries (at least 1).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Pushes an entry, evicting the oldest if at capacity.
    pub fn push(&mut self, entry: ChatEntry) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Pushes a decorated player/disguised message with its trust level.
    pub fn push_player(&mut self, display: Text, trust: MessageTrust) {
        self.push(ChatEntry::Player { display, trust });
    }

    /// Pushes a (non-overlay) system message.
    pub fn push_system(&mut self, content: Text) {
        self.push(ChatEntry::System { content });
    }

    /// Iterates entries oldest-first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &ChatEntry> {
        self.entries.iter()
    }

    /// The most recent entry, if any.
    #[must_use]
    pub fn latest(&self) -> Option<&ChatEntry> {
        self.entries.back()
    }

    /// Number of entries currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the feed is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The maximum number of entries retained.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Clears all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
