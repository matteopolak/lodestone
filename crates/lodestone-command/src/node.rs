//! Node kinds, the arena that owns them, and the tree-construction API.

use std::collections::HashMap;
use std::sync::Arc;

use crate::argument::ArgumentType;

/// An opaque handle into a [`CommandTree`]'s arena. Stable for the lifetime of
/// the tree — nodes are never removed or reindexed once added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) u32);

/// The value produced by parsing one argument slot.
///
/// `Custom` is what a registered [`ArgumentType`] outside the built-in set
/// produces — kept as a plain `String` rather than `Box<dyn Any>` so the type
/// stays `PartialEq`/`Debug` without forcing every custom type to also carry
/// those bounds. A future consumer that needs a richer payload (issue #48's
/// dispatcher, most likely, for something like a resolved player UUID) can
/// widen this enum without touching the parser/reader logic — the two are
/// deliberately decoupled.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedValue {
    Integer(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bool(bool),
    String(String),
    Custom(String),
}

pub(crate) enum NodeKind {
    Root,
    Literal { name: String },
    Argument { name: String, argument_type: Arc<dyn ArgumentType> },
}

impl std::fmt::Debug for NodeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root => write!(f, "Root"),
            Self::Literal { name } => f.debug_struct("Literal").field("name", name).finish(),
            Self::Argument { name, .. } => f.debug_struct("Argument").field("name", name).field("argument_type", &"<dyn ArgumentType>").finish(),
        }
    }
}

pub struct Node {
    pub(crate) kind: NodeKind,
    /// All children, in insertion order — this is the order
    /// [`crate::suggest`] enumerates them in before its final sort, and the
    /// order ambiguous argument-child parses are tried in (see the crate
    /// doc's "known simplifications" section).
    pub(crate) children: Vec<NodeId>,
    /// Literal children only, by exact name, for O(1) exact-token matching.
    pub(crate) literal_children: HashMap<String, NodeId>,
    /// Argument children only, in insertion order.
    pub(crate) argument_children: Vec<NodeId>,
    pub(crate) executable: bool,
    pub(crate) redirect: Option<NodeId>,
    /// Deliberately unconsumed — see the crate doc. Nothing reads this field
    /// yet; it exists purely so #122's per-node permission check has
    /// somewhere to land without changing every node constructor's
    /// signature.
    pub permission: Option<NodeId>,
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("kind", &self.kind)
            .field("children", &self.children)
            .field("executable", &self.executable)
            .field("redirect", &self.redirect)
            .field("permission", &self.permission)
            .finish()
    }
}

impl Node {
    /// The node's own name — `None` for [`NodeKind::Root`].
    pub fn name(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Root => None,
            NodeKind::Literal { name } => Some(name),
            NodeKind::Argument { name, .. } => Some(name),
        }
    }

    pub fn is_executable(&self) -> bool {
        self.executable
    }

    pub fn redirect(&self) -> Option<NodeId> {
        self.redirect
    }

    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    pub(crate) fn argument_type(&self) -> Option<&Arc<dyn ArgumentType>> {
        match &self.kind {
            NodeKind::Argument { argument_type, .. } => Some(argument_type),
            _ => None,
        }
    }

    pub(crate) fn as_literal(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Literal { name } => Some(name),
            _ => None,
        }
    }
}

/// An ECS-free, version-free Brigadier argument tree.
///
/// Owns every [`Node`] in a flat arena addressed by [`NodeId`]; there is no
/// lifetime to thread through callers, and nothing here depends on
/// `lodestone-ecs`, a protocol crate, or any specific Minecraft version — see
/// the crate doc for why that independence is the point.
#[derive(Debug)]
pub struct CommandTree {
    pub(crate) arena: Vec<Node>,
    pub(crate) root: NodeId,
}

impl Default for CommandTree {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandTree {
    pub fn new() -> Self {
        let root = Node {
            kind: NodeKind::Root,
            children: Vec::new(),
            literal_children: HashMap::new(),
            argument_children: Vec::new(),
            executable: false,
            redirect: None,
            permission: None,
        };
        Self { arena: vec![root], root: NodeId(0) }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub(crate) fn node(&self, id: NodeId) -> &Node {
        &self.arena[id.0 as usize]
    }

    pub(crate) fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.arena[id.0 as usize]
    }

    /// Public read access to a node, for callers that want to walk the tree
    /// (e.g. to render it, or to check `permission`/`executable` before
    /// dispatch) without going through `parse`/`suggest`.
    pub fn get(&self, id: NodeId) -> &Node {
        self.node(id)
    }

    fn push_child(&mut self, parent: NodeId, node: Node) -> NodeId {
        let id = NodeId(self.arena.len() as u32);
        self.arena.push(node);
        self.node_mut(parent).children.push(id);
        id
    }

    /// Add a literal child (e.g. `gamemode` in `/gamemode survival`).
    ///
    /// `name` must not contain a space — [`crate::reader::StringReader::peek_token`]
    /// tokenizes on exactly `' '`, which is only equivalent to Brigadier's own
    /// `remaining().starts_with(literal)` check when the literal itself has
    /// none.
    pub fn add_literal(&mut self, parent: NodeId, name: &str) -> NodeId {
        debug_assert!(!name.contains(' '), "literal node names must not contain a space: {name:?}");
        let id = self.push_child(
            parent,
            Node {
                kind: NodeKind::Literal { name: name.to_string() },
                children: Vec::new(),
                literal_children: HashMap::new(),
                argument_children: Vec::new(),
                executable: false,
                redirect: None,
                permission: None,
            },
        );
        self.node_mut(parent).literal_children.insert(name.to_string(), id);
        id
    }

    /// Add an argument child (e.g. `<amount>` in `/give @s stone <amount>`).
    pub fn add_argument(&mut self, parent: NodeId, name: &str, argument_type: Arc<dyn ArgumentType>) -> NodeId {
        let id = self.push_child(
            parent,
            Node {
                kind: NodeKind::Argument { name: name.to_string(), argument_type },
                children: Vec::new(),
                literal_children: HashMap::new(),
                argument_children: Vec::new(),
                executable: false,
                redirect: None,
                permission: None,
            },
        );
        self.node_mut(parent).argument_children.push(id);
        id
    }

    /// Mark whether a node can terminate a command by itself (Brigadier's
    /// "executes" flag — whether the node has a command attached, restated
    /// without the command itself since this crate has no dispatcher, see
    /// the crate doc).
    pub fn set_executable(&mut self, id: NodeId, executable: bool) {
        self.node_mut(id).executable = executable;
    }

    /// Redirect this node to continue parsing from `target`'s children
    /// instead of its own (Brigadier's `fork`/`redirect`, e.g. how every
    /// vanilla `/execute ... run <command>` re-enters the root).
    pub fn set_redirect(&mut self, id: NodeId, target: NodeId) {
        self.node_mut(id).redirect = Some(target);
    }

    pub fn set_permission(&mut self, id: NodeId, permission: Option<NodeId>) {
        self.node_mut(id).permission = permission;
    }
}
