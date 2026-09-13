//! The wire projection: one [`lodestone_command::CommandTree`] to
//! [`lodestone_model::command_tree::CommandTree`].
//!
//! # What this is and what it is not
//!
//! This produces the *content* of `minecraft:commands` (clientbound, id 16) in
//! version-free vocabulary. It encodes no bytes itself — that is
//! `ServerProtocol::encode_commands`, whose `v770` implementation turns this
//! shape into the real packet, and `crate::server`'s join sequence is what sends
//! it. So [`project_filtered`] is the *content* half of a wire that now runs end
//! to end, gated per command against a real vanilla server's captured tree.
//!
//! # Two projections, and why the filtered one is what production sends
//!
//! [`project`] transmits the whole tree; [`project_filtered`] prunes it the way
//! the real send-commands rule does. The real rule never sends a player a
//! node they cannot use: it builds a *copy* through a fill-usable-commands
//! walk, whose recursion into a child's children sits **inside** the "can
//! this source use it" branch, so a denied node takes its whole
//! subtree with it. The unfiltered [`project`] remains because the parity gate
//! wants to compare our declared shape against the real captured one without a
//! permission level in the middle, and because `ServerCommands::wire_tree` is its
//! caller — but the join path uses the filtered one, and a level-0 player really
//! is sent a tree with no `/gamemode` in it at all.
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
//! The wire's restricted flag (`0x20`) is the real "the server would reject
//! this node for a permission-lacking sender" bit. A node carrying a permission requirement
//! sets it — which is what makes the captured `/gamemode` and `/give` root
//! literals `restricted: true` and their children `false`, since the requirement
//! sits on the root literal only and pruning is by subtree.

use std::collections::HashMap;

use lodestone_command::{CommandTree as ServerTree, NodeId, PermissionFilter};
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
        nodes.push(RawCommandNode {
            kind: node_kind(node, wire.get(&id)),
            executable: node.is_executable(),
            restricted: node.permission().is_some(),
            redirect: node.redirect().map(|target| target.index() as usize),
            children: node.children().iter().map(|child| child.index() as usize).collect(),
        });
        index += 1;
    }
    WireTree::new(nodes, tree.root().index() as usize)
}

/// One node's transmitted kind. Shared by both projections so a node cannot be
/// described one way when the whole tree goes out and another way when a
/// permission level prunes it.
///
/// The three-way split is the wire's own: an unnamed node is the root, a named
/// node with a [`WireDescriptor`] is an argument, and a named node without one is
/// a literal. `wire` holds a descriptor for exactly the argument nodes
/// ([`super::Registrar::arg`] is the only writer), so the absence of one *is* the
/// literal test.
fn node_kind(node: &lodestone_command::Node, descriptor: Option<&WireDescriptor>) -> NodeKind {
    match node.name() {
        None => NodeKind::Root,
        Some(name) => match descriptor {
            Some(descriptor) => NodeKind::Argument {
                name: name.to_string(),
                parser: descriptor.parser.clone(),
                suggestions: descriptor.suggestions.clone(),
            },
            None => NodeKind::Literal { name: name.to_string() },
        },
    }
}

/// Project `tree` into the wire shape a *particular* subject may see, pruning
/// every node whose permission `filter` rejects along with its whole subtree.
///
/// This is what the join sequence sends. Two real behaviours are reproduced
/// and they are separate steps:
///
/// * **Which nodes survive** is the real fill-usable-commands walk. The
///   recursion into a child's own children sits inside the "can this source
///   use it" branch, so a permitted node under a denied parent is never
///   reached and never sent. That is why the filter is applied to a node's
///   *permission* only and never to the subtree beneath it — the pruning is
///   structural.
/// * **What index each survivor gets** is the real node-enumeration rule: a
///   breadth-first walk from the root that enqueues a node's children and
///   then its redirect target, assigning ids in visit order. The root
///   therefore lands at 0.
///
/// Indices are *not* the arena's own here, which is the whole difference from
/// [`project`]: dropping a node has to renumber everything after it, and a
/// child/redirect index that still pointed into the arena would silently name a
/// different node. Every index emitted below is looked up in the same
/// enumeration map, never derived from a `NodeId`.
///
/// A redirect whose target did not survive is dropped rather than left dangling,
/// matching the real rule's own lookup returning nothing for an unconverted
/// target.
///
/// # Errors
///
/// [`CommandTreeError`], on the same terms as [`project`] — impossible for a
/// [`super::Registrar`]-built tree, propagated so a future builder that can
/// produce an inconsistent graph fails here rather than on a client.
pub fn project_filtered(
    tree: &ServerTree,
    wire: &HashMap<NodeId, WireDescriptor>,
    filter: &dyn PermissionFilter,
) -> Result<WireTree, CommandTreeError> {
    let root = tree.root();

    // Phase 1: the real fill-usable-commands walk. A node is visible when it
    // is the root, or when its own permission passes *and* its parent is
    // visible.
    let mut visible: std::collections::HashSet<NodeId> = std::collections::HashSet::new();
    visible.insert(root);
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let Some(node) = tree.try_get(id) else { continue };
        for &child in node.children() {
            let allowed = tree.try_get(child).is_some_and(|child| match child.permission() {
                None => true,
                Some(permission) => filter.allows(permission),
            });
            if allowed && visible.insert(child) {
                pending.push(child);
            }
        }
    }

    // Phase 2: the real node-enumeration rule, over the survivors.
    let mut index_of: HashMap<NodeId, usize> = HashMap::new();
    let mut order: Vec<NodeId> = Vec::new();
    let mut queue: std::collections::VecDeque<NodeId> = std::collections::VecDeque::new();
    queue.push_back(root);
    while let Some(id) = queue.pop_front() {
        if index_of.contains_key(&id) {
            continue;
        }
        index_of.insert(id, order.len());
        order.push(id);
        // Every id enqueued below is `visible`, and phase 1 only inserts ids that
        // resolved, so this cannot miss.
        let Some(node) = tree.try_get(id) else { continue };
        for &child in node.children() {
            if visible.contains(&child) {
                queue.push_back(child);
            }
        }
        if let Some(target) = node.redirect() {
            if visible.contains(&target) {
                queue.push_back(target);
            }
        }
    }

    let nodes = order
        .iter()
        .map(|&id| {
            let node = tree.get(id);
            RawCommandNode {
                kind: node_kind(node, wire.get(&id)),
                executable: node.is_executable(),
                restricted: node.permission().is_some(),
                redirect: node.redirect().and_then(|target| index_of.get(&target).copied()),
                children: node
                    .children()
                    .iter()
                    .filter_map(|child| index_of.get(child).copied())
                    .collect(),
            }
        })
        .collect();
    WireTree::new(nodes, index_of[&root])
}
