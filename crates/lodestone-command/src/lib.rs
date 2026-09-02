//! A standalone, ECS-free, version-free Brigadier argument-tree library.
//!
//! ## What it is
//!
//! [`CommandTree`] models a Brigadier-style command tree — root / literal /
//! argument nodes, redirects, and an `executable` flag — plus parsers and
//! suggesters for Brigadier's primitive argument types
//! ([`IntegerArgument`], [`LongArgument`], [`FloatArgument`],
//! [`DoubleArgument`], [`BoolArgument`], [`StringArgument`]), and
//! [`ArgumentTypeRegistry`] for a plugin to register its own. It depends on
//! nothing else in this workspace — no `lodestone-ecs`, no protocol crate, no
//! specific Minecraft version — and nothing else in this workspace depends on
//! it yet.
//!
//! ## This crate is no longer an island
//!
//! It was landed deliberately ahead of its wiring, with three named expected
//! consumers. **One of them now exists:** `lodestone_ecs::commands`
//! builds its trees here, and `lodestone_ecs::permissions` resolves the
//! permissions this crate's [`filter`] seam asks about. The other two
//! remain open and unchanged:
//!
//! - **The server-side Brigadier dispatcher.** **This one now exists
//!   too:** `lodestone_server::commands` defines the `CommandSource`, the
//!   executor and *modifier* tables, and the fork-aware dispatch walk, all
//!   still *outside* this crate's arena — `executable` remains a bare flag
//!   here, and both dispatchers keep their callbacks in a `NodeId`-keyed side
//!   table so two dispatchers over one tree library cannot disagree about
//!   where a callback lives. The one thing that dispatcher needed *from* this
//!   crate is [`ParsedValue::Dyn`], added for it.
//! - **The client command UX.** `CommandTree::suggest` exists for it, but
//!   the client command UX shipped against `lodestone_model::command_tree`
//!   instead; see `docs/plugin-commands.md` for why that duplication is the
//!   right call and what was collapsed instead.
//!
//! ## The `permission` field, and its corrected type
//!
//! Every [`Node`] carries `pub permission: Option<String>`
//! ([`CommandTree::set_permission`], [`CommandTree::require_permission`]), and
//! it **is** read now — by [`CommandTree::parse_filtered`] and
//! [`CommandTree::suggest_filtered`], against a caller-supplied
//! [`PermissionFilter`].
//!
//! It was reserved as `Option<NodeId>`, which was the wrong type: a `NodeId` is
//! a handle into *this tree's own arena*, and a permission node is a dotted
//! string (`myplugin.admin`) with nothing in a command tree to point at. The
//! field was never read, so correcting it broke no caller — but it is a good
//! example of a reservation that looked right and would have had to change
//! anyway.
//!
//! Gating is not applied by this crate's own resolution: it has no
//! dependencies and cannot know what a player is. See [`filter`] for the two
//! *different* behaviours a denied node gets (loud on parse, silent on
//! suggest) and why vanilla needs neither.
//!
//! ## What this crate does *not* do
//!
//! - **No decode of `COMMANDS` (packet id 16) or `COMMAND_SUGGESTIONS` (id
//!   15).** Verified directly: `grep -rn "clientbound::COMMANDS"
//!   crates/protocol/v770/src/adapter.rs` returns zero hits, even though the
//!   packet id constants exist in the generated tables
//!   (`crates/protocol/v770/src/generated/packet_ids.rs`). Both packets have
//!   **zero decode** in every protocol family in this workspace today. This
//!   crate does not add any — that is explicitly someone else's question
//!   (owned by whoever is currently working `chat.rs`/`lodestone-client`),
//!   and reaching into `crates/protocol/**` was out of scope here
//!   regardless.
//! - **No ECS registry, no dispatcher, no plugin API.** Those are separate
//!   tracked work items, plus the client-input-interception work; this
//!   crate is the argument-tree substrate underneath all three, not any of
//!   them.
//! - **Minecraft-flavoured argument types** (player name, block id, entity
//!   selector, `BlockPos`, ...) are not built in. They depend on this
//!   substrate; [`ArgumentTypeRegistry`] is exactly the
//!   extension point for them.
//!
//! ## Known simplifications versus real Brigadier
//!
//! Ported from `com.mojang.brigadier` 1.3.10 (decompiled sources at
//! the decompiled tree's command-argument synchronization package plus
//! the upstream `Mojang/brigadier` sources for the parts Mojang doesn't
//! subclass — `StringReader`, `CommandDispatcher`, `Suggestions`), with two
//! deliberate departures documented where they matter most:
//!
//! - [`CommandTree::parse`] tries argument children in insertion order and
//!   takes the first success, rather than collecting every simultaneously
//!   successful candidate and preferring a complete parse among them the way
//!   `CommandDispatcher::parseNodes` does. This only differs when one node
//!   has more than one *argument* child that both accept the same text —
//!   none of the three named consumers' expected trees do that.
//! - [`CommandTree::parse`] collapses Brigadier's `reader.canRead(redirect ==
//!   null ? 2 : 1)` recursion gate to a single `can_read()` check. Getting
//!   *some* form of this gate right turns out to matter a lot more than it
//!   looks: it is the reason a redirect back to an ancestor is merely *deep*
//!   rather than *infinite* in real Brigadier — every redirect hop requires
//!   and then consumes at least one separator character before recursing
//!   again, so recursion depth is always bounded by the input's length, for
//!   *any* tree shape, cyclic-looking ones included. A first pass at this
//!   crate got that gate wrong (skipped the separator and followed the
//!   redirect even at true end-of-input) and it was caught by a test
//!   expecting termination, not a hang — see `tests/brigadier_spec.rs`.
//! - Given the above, `CommandTree::parse` *additionally* rejects a redirect
//!   that would land on a `(node, cursor)` pair already visited on the
//!   current path. This is not needed to stop an ordinary Brigadier-shaped
//!   cycle (the consumption gate already does that) — it exists because a
//!   custom [`ArgumentType`] (the plugin extension point) receives
//!   `&mut StringReader` and nothing stops a buggy or adversarial one from
//!   moving the cursor *backward*, defeating the consumption-gate's
//!   guarantee from outside this module. `tests/brigadier_spec.rs` exercises
//!   this with exactly such a type, plus a control using a well-behaved type
//!   on a structurally identical tree to show the guard doesn't misfire on
//!   legitimate repeated redirects (e.g. the `/execute ... run <command>`
//!   pattern, which really does redirect back to the root on every hop).
//!
//! Positions in every [`error::ParseError`] are `char` offsets, not bytes —
//! see [`reader`]'s module doc for why, and for two easy-to-miss cases where
//! the *reported* position is not where you'd guess (an out-of-range number
//! reports the start of the token, not its end; an invalid escape reports the
//! escaped character itself, not one past it).

pub mod argument;
pub mod error;
pub mod filter;
pub mod node;
pub mod parse;
pub mod reader;
pub mod suggest;

pub use argument::{ArgumentType, ArgumentTypeRegistry, BoolArgument, ChoicesArgument, DoubleArgument, FloatArgument, IntegerArgument, LongArgument, StringArgument, StringKind, SuggestionProvider};
pub use error::{ParseError, ParseErrorKind};
pub use filter::{AllowAll, DenyAll, PermissionFilter};
pub use node::{AnyValue, CommandTree, Node, NodeId, ParsedValue};
pub use parse::ParsedCommand;
pub use reader::StringReader;
