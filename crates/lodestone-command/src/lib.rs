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
//! ## This is a declared island, on purpose
//!
//! This crate has **zero consumers today**. That is a sanctioned outcome
//! here, not an oversight: per the architecture review that produced this
//! crate, it is capability landed ahead of its wiring, and the follow-up work
//! is named rather than left implicit (an *unnamed* island is this repo's
//! most common defect class — see `CLAUDE.md`). Three consumers are expected,
//! none of which exist yet and none of which this crate builds:
//!
//! - **#48** — the server-side Brigadier dispatcher. This crate has no
//!   `CommandSource`, no command callback, and no execution semantics at
//!   all — `executable` is a bare flag with nothing attached to run.
//! - **#46** — the client command UX (autocomplete, inline highlighting).
//!   `CommandTree::suggest` exists specifically so #46 has something to call
//!   once it decodes a tree off the wire, but this crate builds no tree from
//!   network bytes — see "What this crate does *not* do" below.
//! - **#118** — plugin command registration. Its own text says the plugin
//!   registry and #48's dispatcher "should share rather than diverge" — this
//!   crate is that shared argument-tree substrate for both.
//!
//! ## The `permission` field
//!
//! Every [`Node`] carries `pub permission: Option<NodeId>`
//! ([`CommandTree::set_permission`]). **Nothing reads it.** It is here from
//! day one, unconsumed, so that #122's per-node permission check has
//! somewhere to land without changing every node constructor's signature
//! when it arrives — the field, not a real permission system, is the
//! deliverable.
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
//!   (issue #46, and the agent currently working `chat.rs`/`lodestone-client`
//!   owns it), and reaching into `crates/protocol/**` was out of scope here
//!   regardless.
//! - **No ECS registry, no dispatcher, no plugin API.** Those are #118, #48
//!   and the client-input-interception work respectively; this crate is the
//!   argument-tree substrate underneath all three, not any of them.
//! - **Minecraft-flavoured argument types** (player name, block id, entity
//!   selector, `BlockPos`, ...) are not built in. Issue #119 lists them as
//!   depending on this substrate; [`ArgumentTypeRegistry`] is exactly the
//!   extension point for them.
//!
//! ## Known simplifications versus real Brigadier
//!
//! Ported from `com.mojang.brigadier` 1.3.10 (decompiled sources at
//! `.cache/mc/26.2/src/net/minecraft/commands/synchronization/brigadier` plus
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
//!   custom [`ArgumentType`] (issue #119's own extension point) receives
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
pub mod node;
pub mod parse;
pub mod reader;
pub mod suggest;

pub use argument::{ArgumentType, ArgumentTypeRegistry, BoolArgument, DoubleArgument, FloatArgument, IntegerArgument, LongArgument, StringArgument, StringKind};
pub use error::{ParseError, ParseErrorKind};
pub use node::{CommandTree, Node, NodeId, ParsedValue};
pub use parse::ParsedCommand;
pub use reader::StringReader;
