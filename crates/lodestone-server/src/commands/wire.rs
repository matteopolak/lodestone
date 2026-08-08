//! The wire projection: one [`lodestone_command::CommandTree`] to
//! [`lodestone_model::command_tree::CommandTree`].
//!
//! # What this is and what it is not
//!
//! This produces the *content* of `minecraft:commands` (clientbound, id 16) in
//! version-free vocabulary. It does **not** encode any bytes and nothing sends
//! it yet: the encoder is a later unit, and no protocol family in this workspace
//! has a `COMMANDS` encode arm today. So tab completion against the server's real
//! tree does not work end to end — what works now is that the projection exists
//! and is gated, per command, against a real vanilla server's captured tree.
//!
//! # Why the projection is not the source of truth for a node's identity
//!
//! It cannot invent anything. Every argument node's parser and suggestion
//! provider come from the [`WireDescriptor`] that
//! [`super::Registrar::arg`] recorded from the [`lodestone_command_mc::McArg`]
//! *in the same call that installed the parser*. This walker only assigns
//! indices and copies flags. That is the whole defence against a transmitted
//! tree that disagrees with the executing one.
//!
//! # `restricted` is derived, not declared
//!
//! `FLAG_RESTRICTED` (`0x20`) is vanilla's "the server would reject this node
//! for a permission-lacking sender". A node carrying a permission requirement
//! sets it — which is what makes the captured `/gamemode` and `/give` root
//! literals `restricted: true` and their children `false`, since the requirement
//! sits on the root literal only and pruning is by subtree.

use std::collections::HashMap;

use lodestone_command::{CommandTree as ServerTree, NodeId};
use lodestone_model::command_tree::{
    CommandTree as WireTree, CommandTreeError, NodeKind, RawCommandNode,
};

use super::registrar::WireDescriptor;

/// Project `tree` into the version-free wire shape.
///
/// # Errors
///
/// [`CommandTreeError`] if the produced index graph is inconsistent, which cannot
/// happen for a tree built by [`super::Registrar`] — the arena is append-only and
/// every `NodeId` is in range by construction. Propagated rather than unwrapped
/// so a future builder that *can* produce one fails loudly here.
pub fn project(
    tree: &ServerTree,
    wire: &HashMap<NodeId, WireDescriptor>,
) -> Result<WireTree, CommandTreeError> {
    // The arena is dense and starts at the root, so arena index *is* wire index.
    // Stated rather than assumed: `NodeId`s are handed out by `arena.len()` and
    // never removed (`lodestone_command::node`'s own doc), so a walk that
    // renumbered would only add a way to get it wrong.
    let mut nodes: Vec<RawCommandNode> = Vec::new();
    let mut index = 0u32;
    loop {
        let id = NodeId::from_index(index);
        let Some(node) = tree.try_get(id) else { break };
        let kind = match node.name() {
            None => NodeKind::Root,
            Some(name) => match wire.get(&id) {
                Some(descriptor) => NodeKind::Argument {
                    name: name.to_string(),
                    parser: descriptor.parser.clone(),
                    suggestions: descriptor.suggestions.clone(),
                },
                None => NodeKind::Literal { name: name.to_string() },
            },
        };
        nodes.push(RawCommandNode {
            kind,
            executable: node.is_executable(),
            restricted: node.permission().is_some(),
            redirect: node.redirect().map(|target| target.index() as usize),
            children: node.children().iter().map(|child| child.index() as usize).collect(),
        });
        index += 1;
    }
    WireTree::new(nodes, tree.root().index() as usize)
}
