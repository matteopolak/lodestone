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

use lodestone_model::{Text, TextSpan, TextStyle};
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

/// The display component of a feed entry, regardless of variant.
fn entry_display(entry: &ChatEntry) -> &Text {
    match entry {
        ChatEntry::Player { display, .. } => display,
        ChatEntry::System { content } => content,
    }
}

/// A [`ChatFeed`] plus each entry's **arrival time**, which is what a renderer
/// needs to fade a line out.
///
/// The message *content* model — bounding, ordering, trust, the 100-line cap —
/// is [`ChatFeed`]; this adds only the monotonic arrival time of each entry,
/// which drives the vanilla fade-out (a client-renderer detail vanilla itself
/// keeps in `ChatComponent`, not in server state). The two structures are pushed
/// and evicted in lockstep so index *i* of one matches the other.
///
/// Times are plain `f64` seconds supplied by the caller, so this type stays free
/// of any clock (and thus wasm-safe and unit-testable without a real time
/// source). In the shell that clock is
/// `lodestone_ecs::FrameClock` — see `docs/sim-dissolution.md` on why the log
/// and the clock had to move together.
///
/// # Why this lives in `lodestone-game` rather than the shell
///
/// It used to be `lodestone_shell::chat::ChatLog`. `docs/bevy-migration.md`
/// Stage 5 makes it the payload of a `lodestone_ecs::SessionChat` component, and
/// `lodestone-ecs` cannot depend on `lodestone-shell` (the dependency runs the
/// other way). It sits beside the feed it wraps for the same reason every other
/// session aggregate does (§8: "`lodestone-game`'s folds stay plain functions
/// the ECS calls").
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

    /// The entries themselves, oldest-first — the only way to reach a
    /// [`ChatEntry`]'s own fields (its [`MessageTrust`], or whether it is a
    /// system message at all) from outside this module.
    ///
    /// Every `recent_*` projection above flattens an entry to text and drops
    /// that classification, which is why a caller wanting to *badge* a line —
    /// vanilla's `GuiMessageTag` — cannot be served by them.
    #[must_use]
    pub fn feed(&self) -> &ChatFeed {
        &self.feed
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

    /// The most recent `n` lines paired with their **age** in seconds relative to
    /// `now`, which is the shape the HUD's fade-out consumes.
    ///
    /// A line stamped in the future (only reachable if a caller passes a clock
    /// that went backwards) reads as age `0.0` rather than negative.
    #[must_use]
    pub fn recent_ages(&self, n: usize, now: f64) -> Vec<(String, f32)> {
        self.recent(n)
            .into_iter()
            .map(|(line, at)| (line, (now - at).max(0.0) as f32))
            .collect()
    }

    /// The most recent `n` lines' full styled spans, oldest-first, paired with
    /// each entry's arrival timestamp — the span sibling of
    /// [`recent`](Self::recent): same walk, `to_spans()` in place of
    /// `to_legacy_string()`, so a colour `to_legacy_string` cannot represent
    /// (any `TextColor::Rgb`) survives.
    ///
    /// `recent`'s own flattening is the loss point named in
    /// `docs/text-colour.md`'s "Chat is still hex-blind" section: `ChatEntry`
    /// already stores a full [`Text`] per entry, so nothing about storage
    /// changes here, only how one is read out.
    #[must_use]
    pub fn recent_spans(&self, n: usize) -> Vec<(Vec<TextSpan>, f64)> {
        let start = self.feed.len().saturating_sub(n);
        self.feed
            .iter()
            .zip(self.times.iter())
            .skip(start)
            .map(|(entry, at)| (entry_display(entry).to_spans(), *at))
            .collect()
    }

    /// The most recent `n` lines' styled spans paired with their **age** in
    /// seconds relative to `now` — the span sibling of
    /// [`recent_ages`](Self::recent_ages): same "age relative to now, floored
    /// at zero" projection, composed over [`recent_spans`](Self::recent_spans)
    /// instead of [`recent`](Self::recent).
    #[must_use]
    pub fn recent_ages_spans(&self, n: usize, now: f64) -> Vec<(Vec<TextSpan>, f32)> {
        self.recent_spans(n)
            .into_iter()
            .map(|(spans, at)| (spans, (now - at).max(0.0) as f32))
            .collect()
    }

    /// The most recent `n` lines' [`crate::text::InteractiveSpan`]s — the
    /// `click`/`hover`-carrying sibling of [`recent_spans`](Self::recent_spans),
    /// paired with each entry's arrival timestamp.
    ///
    /// `recent_spans` cannot supply this: `Text::to_spans()`'s output type has
    /// nowhere to put `click`/`hover`, which is why a chat HUD built only on
    /// `recent_ages_spans` can never hit-test a link or a hover tooltip no
    /// matter how carefully it is written — the field is gone two calls
    /// upstream of the draw. `translate` is threaded through (unlike
    /// `recent_spans`, which resolves through the model's built-in stub table
    /// only) so a hit-tested run's text matches what a real language pack
    /// actually drew, the same correction `tab_list_view` already makes over
    /// its own `Text::to_spans()`-only predecessor.
    #[must_use]
    pub fn recent_interactive(
        &self,
        n: usize,
        translate: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<(Vec<crate::text::InteractiveSpan>, f64)> {
        let start = self.feed.len().saturating_sub(n);
        self.feed
            .iter()
            .zip(self.times.iter())
            .skip(start)
            .map(|(entry, at)| (crate::text::interactive_spans(entry_display(entry), translate), *at))
            .collect()
    }

    /// The `age`-relative sibling of [`recent_interactive`](Self::recent_interactive),
    /// matching [`recent_ages_spans`](Self::recent_ages_spans)'s own projection.
    #[must_use]
    pub fn recent_ages_interactive(
        &self,
        n: usize,
        now: f64,
        translate: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<(Vec<crate::text::InteractiveSpan>, f32)> {
        self.recent_interactive(n, translate)
            .into_iter()
            .map(|(spans, at)| (spans, (now - at).max(0.0) as f32))
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

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn log_keeps_newest_and_bounds_length() {
        let mut log = ChatLog::new();
        for i in 0..(DEFAULT_CHAT_CAPACITY + 10) {
            log.push_system(Text::literal(format!("line {i}")), i as f64);
        }
        assert_eq!(
            log.len(),
            DEFAULT_CHAT_CAPACITY,
            "log must evict oldest at capacity"
        );
        let recent: Vec<String> = log.recent(3).into_iter().map(|(line, _)| line).collect();
        // The three newest survive, oldest-first.
        assert_eq!(
            recent,
            [
                format!("line {}", DEFAULT_CHAT_CAPACITY + 7),
                format!("line {}", DEFAULT_CHAT_CAPACITY + 8),
                format!("line {}", DEFAULT_CHAT_CAPACITY + 9),
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

    /// The age projection is what the HUD actually reads, so it gets its own
    /// assertion rather than being assumed to follow from `recent`.
    #[test]
    fn recent_ages_subtracts_the_supplied_clock() {
        let mut log = ChatLog::new();
        log.push_system(Text::literal("old"), 1.0);
        log.push_system(Text::literal("new"), 9.0);
        assert_eq!(
            log.recent_ages(2, 10.0),
            vec![("old".to_string(), 9.0), ("new".to_string(), 1.0)]
        );
    }

    /// A clock that went backwards must not produce a negative age — the HUD
    /// feeds it straight into a fade curve.
    #[test]
    fn a_line_stamped_in_the_future_reads_as_age_zero() {
        let mut log = ChatLog::new();
        log.push_system(Text::literal("ahead"), 5.0);
        assert_eq!(log.recent_ages(1, 1.0), vec![("ahead".to_string(), 0.0)]);
    }

    /// The discriminating input for this whole gap: an RGB colour has no
    /// legacy-code equivalent (`TextColor::legacy_code` returns `None` for
    /// it), so `recent`'s own flattening carries no colour at all for it —
    /// this is what makes chat "hex-blind" and what a named colour could
    /// never catch. `recent_spans` must preserve it.
    #[test]
    fn recent_spans_preserves_a_hex_colour_the_legacy_string_cannot_represent() {
        let mut log = ChatLog::new();
        let mut styled = Text::literal("hex");
        styled.style.color = Some(lodestone_model::TextColor::Rgb(0x1a_2b3c));
        log.push_system(styled, 1.0);

        let (line, _) = &log.recent(1)[0];
        assert_eq!(
            line, "hex",
            "the legacy string is the control: it must show the loss, not accidentally dodge it"
        );

        let spans = log.recent_spans(1);
        assert_eq!(spans.len(), 1);
        let (spans, at) = &spans[0];
        assert_eq!(*at, 1.0);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].style.color,
            Some(lodestone_model::TextColor::Rgb(0x1a_2b3c)),
            "the hex colour must survive the span read"
        );
    }

    /// [`ChatLog::recent_ages_spans`] is [`ChatLog::recent_ages`]'s own
    /// "age relative to now" projection composed over
    /// [`ChatLog::recent_spans`] rather than [`ChatLog::recent`] — checked
    /// against the same clock arithmetic `recent_ages_subtracts_the_supplied_clock`
    /// already pins, with a hex colour riding along to confirm the span path
    /// is really what is under the composition.
    #[test]
    fn recent_ages_spans_ages_like_recent_ages_and_keeps_the_colour() {
        let mut log = ChatLog::new();
        let mut old = Text::literal("old");
        old.style.color = Some(lodestone_model::TextColor::Rgb(0x10_2030));
        log.push_system(old, 1.0);
        let mut new = Text::literal("new");
        new.style.color = Some(lodestone_model::TextColor::Aqua);
        log.push_system(new, 9.0);

        let out = log.recent_ages_spans(2, 10.0);
        assert_eq!(out.len(), 2);
        let (old_spans, old_age) = &out[0];
        assert_eq!(*old_age, 9.0, "must match recent_ages's own arithmetic exactly");
        assert_eq!(old_spans[0].style.color, Some(lodestone_model::TextColor::Rgb(0x10_2030)));
        let (new_spans, new_age) = &out[1];
        assert_eq!(*new_age, 1.0);
        assert_eq!(new_spans[0].style.color, Some(lodestone_model::TextColor::Aqua));
    }

    /// The whole point of [`ChatLog::recent_interactive`]: a `click_event` on
    /// a logged message survives all the way out through the log, which
    /// [`recent_spans`] (used by every existing chat-render call site) cannot
    /// do — its `TextSpan` output has no field for it. Same ageing arithmetic
    /// as [`recent_ages_spans_ages_like_recent_ages_and_keeps_the_colour`],
    /// with a `click` riding along instead of a colour.
    #[test]
    fn recent_ages_interactive_ages_like_recent_ages_and_keeps_the_click() {
        use lodestone_model::text::{ClickAction, ClickEvent};

        let mut log = ChatLog::new();
        let mut msg = Text::literal("visit");
        msg.click = Some(ClickEvent {
            action: ClickAction::OpenUrl,
            value: "https://example.invalid/".to_string(),
        });
        log.push_system(msg, 1.0);
        // A negative control alongside it: a message with no click must come
        // back with `None`, not some fabricated default.
        log.push_system(Text::literal("plain"), 9.0);

        let out = log.recent_ages_interactive(2, 10.0, &|_: &str| None);
        assert_eq!(out.len(), 2);
        let (linked_spans, linked_age) = &out[0];
        assert_eq!(linked_age, &9.0, "must match recent_ages's own arithmetic exactly");
        assert_eq!(
            linked_spans[0].click,
            Some(ClickEvent {
                action: ClickAction::OpenUrl,
                value: "https://example.invalid/".to_string()
            })
        );
        let (plain_spans, plain_age) = &out[1];
        assert_eq!(plain_age, &1.0);
        assert_eq!(plain_spans[0].click, None);
    }
}
