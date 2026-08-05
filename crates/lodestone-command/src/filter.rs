//! Per-node permission gating (issue #122): the [`PermissionFilter`] seam and
//! the two behaviours vanilla and Bukkit give a gated node.
//!
//! This crate cannot resolve a permission — it has no dependencies and no idea
//! what a player is. A caller supplies a filter; every node whose
//! [`crate::Node::permission`] the filter rejects becomes invisible, **along
//! with its entire subtree**.
//!
//! # Subtree pruning is vanilla's actual semantics, not a shortcut
//!
//! From 26.2's `Commands.fillUsableCommands`
//! (`.cache/mc/26.2/src/net/minecraft/commands/Commands.java:421`):
//!
//! ```text
//! for (CommandNode<S> child : source.getChildren()) {
//!    if (child.canUse(commandFilter)) {
//!       ...
//!       target.addChild(node);
//!       if (!child.getChildren().isEmpty()) {
//!          fillUsableCommands(child, node, commandFilter, converted);
//!       }
//!    }
//! }
//! ```
//!
//! The recursion into a child's own children sits **inside** the `canUse`
//! branch. So a permitted node under a denied parent is never reached and
//! never sent — the whole branch disappears from the tree the client is given,
//! which is why a gated branch simply does not appear in tab completion. This
//! module reproduces that by pruning during the walk rather than by building a
//! filtered copy of the tree: same visibility, no allocation per query.
//!
//! # Execution and suggestion are gated *differently*, on purpose
//!
//! Issue #122 asks for both halves, and they are not the same behaviour:
//!
//! | operation | a denied node is... | why |
//! |---|---|---|
//! | [`crate::CommandTree::parse_filtered`] | an explicit [`crate::ParseErrorKind::NoPermission`] | Bukkit answers a permission-gated command with "you do not have permission", not "unknown command" — the player needs to know the command exists and is not theirs |
//! | [`crate::CommandTree::suggest_filtered`] | silently absent | vanilla never sent the node, so the client cannot suggest it; a suggestion that leaked the node's *existence* would defeat the gate |
//!
//! Getting these the same way round is the easy mistake: making `parse` silent
//! too is indistinguishable from a typo to the player, and making `suggest`
//! loud leaks the command tree to everyone.
//!
//! # How to change it
//!
//! The ungated [`crate::CommandTree::parse`] and
//! [`crate::CommandTree::suggest`] are **defined as** the filtered versions
//! with [`AllowAll`], rather than being separate implementations. Keep it that
//! way: two walks would diverge, and the ungated one is the one every existing
//! test exercises, so the gated one would be the one that rotted.

/// Resolves whether a subject holds a permission node.
///
/// Implemented blanket-style for any `Fn(&str) -> bool`, so a caller can pass
/// a closure. `lodestone_ecs::commands` passes one that consults
/// `lodestone_ecs::permissions::Permissions`.
pub trait PermissionFilter {
    /// Does the subject hold `permission`? Called only for nodes that actually
    /// carry one — an unrestricted node never reaches here.
    fn allows(&self, permission: &str) -> bool;
}

impl<F> PermissionFilter for F
where
    F: Fn(&str) -> bool,
{
    fn allows(&self, permission: &str) -> bool {
        self(permission)
    }
}

/// The permissive filter. [`crate::CommandTree::parse`] and
/// [`crate::CommandTree::suggest`] are the filtered walks with this, so there
/// is exactly one implementation of each.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowAll;

impl PermissionFilter for AllowAll {
    fn allows(&self, _permission: &str) -> bool {
        true
    }
}

/// The opposite of [`AllowAll`], for tests and for a "what would a
/// no-permissions subject see?" query. Denies every node that carries a
/// permission at all, leaving only unrestricted nodes visible.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyAll;

impl PermissionFilter for DenyAll {
    fn allows(&self, _permission: &str) -> bool {
        false
    }
}
