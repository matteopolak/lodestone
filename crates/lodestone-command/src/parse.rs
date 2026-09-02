//! `CommandTree::parse` — a restatement of
//! `CommandDispatcher::{parseNodes,execute}` (brigadier 1.3.10) without a
//! `CommandSource`, a command callback, or ambiguity-preserving backtracking
//! across multiple simultaneously-successful argument children. See the
//! crate doc's "known simplifications" section for exactly what that last one
//! gives up, and why it doesn't matter for any tree this crate's three named
//! future consumers are expected to build.

use crate::error::{ParseError, ParseErrorKind};
use crate::filter::{AllowAll, PermissionFilter};
use crate::node::{CommandTree, NodeId, ParsedValue};
use crate::reader::StringReader;

/// The result of a successful [`CommandTree::parse`]: the path of nodes taken
/// (following redirects) and the arguments collected along the way, in the
/// order they were parsed.
#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub nodes: Vec<NodeId>,
    pub arguments: Vec<(String, ParsedValue)>,
}

impl ParsedCommand {
    pub fn argument(&self, name: &str) -> Option<&ParsedValue> {
        self.arguments.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

impl CommandTree {
    /// Parse `input` against this tree, exactly as far as
    /// `CommandDispatcher::execute` would: literal children are matched
    /// exactly (by token, per [`StringReader::peek_token`]); failing that,
    /// argument children are tried in insertion order and the **first**
    /// success wins — real Brigadier instead collects every simultaneously
    /// successful candidate and prefers a complete parse with no exceptions
    /// among them, which only differs from "first success wins" when a
    /// single node has more than one *argument* child that both accept the
    /// same text. None of this crate's tests, nor any tree its three named
    /// consumers are expected to build, do that.
    ///
    /// A matched node followed by more input must be followed by exactly one
    /// `' '` (Brigadier's `ARGUMENT_SEPARATOR_CHAR`), which is then consumed;
    /// redirects are followed to their target's children; leftover input at
    /// the end is `UnknownArgument` (something matched, but not everything)
    /// or `UnknownCommand` (nothing at the root matched at all).
    pub fn parse(&self, input: &str) -> Result<ParsedCommand, ParseError> {
        self.parse_filtered(input, &AllowAll)
    }

    /// [`CommandTree::parse`], with per-node permission gating.
    ///
    /// A node whose [`crate::Node::permission`] `filter` rejects is treated as
    /// absent, together with its whole subtree — vanilla's
    /// `fillUsableCommands` semantics, see [`crate::filter`]. When a token
    /// matched such a node and nothing else could take it, the error is
    /// [`ParseErrorKind::NoPermission`] naming the node required, **not**
    /// `UnknownCommand`: Bukkit tells the player the command is not theirs
    /// rather than pretending it does not exist. Suggestion is the opposite —
    /// see [`CommandTree::suggest_filtered`].
    pub fn parse_filtered(
        &self,
        input: &str,
        filter: &dyn PermissionFilter,
    ) -> Result<ParsedCommand, ParseError> {
        let mut reader = StringReader::new(input);
        let mut visited_redirects: Vec<(NodeId, usize)> = Vec::new();
        let mut nodes = Vec::new();
        let mut arguments = Vec::new();

        self.parse_nodes(
            self.root,
            &mut reader,
            &mut visited_redirects,
            &mut nodes,
            &mut arguments,
            filter,
        )?;

        if reader.can_read() {
            let kind = if nodes.is_empty() { ParseErrorKind::UnknownCommand } else { ParseErrorKind::UnknownArgument };
            return Err(ParseError::new(reader.cursor(), kind));
        }

        let last = nodes.last().copied().unwrap_or(self.root);
        if !self.node(last).executable {
            return Err(ParseError::new(reader.cursor(), ParseErrorKind::NotExecutable));
        }

        Ok(ParsedCommand { nodes, arguments })
    }

    /// Is this node visible to the filter? A node with no permission always
    /// is. Denial prunes the node *and* its subtree, because a walk that never
    /// steps onto the node can never reach its children.
    pub(crate) fn node_allowed(&self, id: NodeId, filter: &dyn PermissionFilter) -> bool {
        match self.node(id).permission.as_deref() {
            None => true,
            Some(permission) => filter.allows(permission),
        }
    }

    fn parse_nodes(
        &self,
        node_id: NodeId,
        reader: &mut StringReader,
        visited_redirects: &mut Vec<(NodeId, usize)>,
        nodes: &mut Vec<NodeId>,
        arguments: &mut Vec<(String, ParsedValue)>,
        filter: &dyn PermissionFilter,
    ) -> Result<(), ParseError> {
        if !reader.can_read() {
            return Ok(());
        }

        let node = self.node(node_id);

        // A token that matched a node the filter denies, remembered so the
        // error can be `NoPermission` rather than `UnknownCommand` if nothing
        // else in this position accepts the token. See `crate::filter` for why
        // parse is loud where suggest is silent.
        let mut denied: Option<String> = None;

        // Literal exact match takes priority over any argument child, and is
        // unambiguous by construction (`add_literal` rejects names with a
        // space) — this is `LiteralCommandNode::parse`'s
        // `remaining().startsWith(literal)` + trailing-separator check,
        // restated as "the token up to the next space equals the literal".
        let token = reader.peek_token();
        if let Some(&literal_child) = node.literal_children.get(token.as_str()) {
            if self.node_allowed(literal_child, filter) {
                reader.advance(token.chars().count());
                nodes.push(literal_child);
                return self.after_match(
                    literal_child,
                    reader,
                    visited_redirects,
                    nodes,
                    arguments,
                    filter,
                );
            }
            denied = self.node(literal_child).permission.clone();
        }

        // No literal matched: try argument children in insertion order.
        // Brigadier reports the *specific* argument error (e.g. "invalid
        // integer") rather than a generic unknown-command/argument error
        // whenever exactly one child was attempted and failed with an
        // exception (`ParseResults.getExceptions().size() == 1`) — we
        // approximate that by remembering the last failure and surfacing it
        // if no child in this position succeeded.
        let mut last_error: Option<ParseError> = None;
        for &child_id in &node.argument_children {
            let checkpoint = reader.cursor();
            let argument_type = self.node(child_id).argument_type().expect("argument_children only holds Argument nodes");
            match argument_type.parse(reader) {
                Ok(value) => {
                    // The permission check happens *after* a successful parse
                    // rather than before it, so a denied argument node does
                    // not shadow a later sibling that would have accepted the
                    // same token. Restoring the cursor is required: the
                    // argument type moved it.
                    if !self.node_allowed(child_id, filter) {
                        reader.set_cursor(checkpoint);
                        if denied.is_none() {
                            denied = self.node(child_id).permission.clone();
                        }
                        continue;
                    }
                    let name = self.node(child_id).name().expect("Argument nodes always have a name").to_string();
                    arguments.push((name, value));
                    nodes.push(child_id);
                    return self.after_match(
                        child_id,
                        reader,
                        visited_redirects,
                        nodes,
                        arguments,
                        filter,
                    );
                }
                Err(e) => {
                    reader.set_cursor(checkpoint);
                    last_error = Some(e);
                }
            }
        }

        // A denied match outranks a failed parse: the player's problem is the
        // permission, and reporting "invalid integer" for a branch they cannot
        // use would be actively misleading.
        if let Some(permission) = denied {
            return Err(ParseError::new(
                reader.cursor(),
                ParseErrorKind::NoPermission { permission },
            ));
        }

        if let Some(e) = last_error {
            return Err(e);
        }

        // Truly nothing here could take this token — leave the reader where
        // it is; the top-level caller turns "there's still input left" into
        // UnknownCommand/UnknownArgument depending on whether anything
        // matched earlier in the path.
        Ok(())
    }

    /// After a child successfully consumed a token: enforce the
    /// argument-separator boundary, then either follow a redirect, recurse
    /// into the child's own children, or stop (a leaf).
    fn after_match(
        &self,
        child_id: NodeId,
        reader: &mut StringReader,
        visited_redirects: &mut Vec<(NodeId, usize)>,
        nodes: &mut Vec<NodeId>,
        arguments: &mut Vec<(String, ParsedValue)>,
        filter: &dyn PermissionFilter,
    ) -> Result<(), ParseError> {
        // `CommandDispatcher::parseNodes` only bothers skipping the separator
        // and recursing (into a redirect target, or into the child's own
        // children) when there is *strictly more* input left to justify it —
        // `reader.canRead(child.getRedirect() == null ? 2 : 1)`, collapsed
        // here to a single `can_read()` check (a documented simplification:
        // real Brigadier additionally declines to recurse into a *non*-redirect
        // child's own children when only the bare separator remains with
        // nothing after it, which this crate treats the same as true EOF).
        // Getting this gate right is exactly what makes a redirect back to an
        // ancestor merely *deep* rather than *infinite*: every redirect hop
        // requires and then consumes at least this one separator character,
        // so recursion depth is always bounded by the input's length. A first
        // pass at this function skipped the separator and followed the
        // redirect unconditionally, which broke exactly this guarantee for a
        // trailing zero-width match at true end-of-input — caught by
        // `tests/brigadier_spec.rs`'s non-cyclic redirect control failing
        // before this comment existed.
        if !reader.can_read() {
            return Ok(());
        }
        if reader.peek() != Some(' ') {
            return Err(ParseError::new(reader.cursor(), ParseErrorKind::ExpectedArgumentSeparator));
        }
        reader.skip();

        if let Some(target) = self.node(child_id).redirect() {
            let key = (target, reader.cursor());
            // A redirect that lands on a `(node, cursor)` pair already on
            // this path consumed nothing on the way back to it, and would do
            // so again forever — this is the guard `tests/brigadier_spec.rs`
            // exercises directly; real Brigadier has no equivalent and would
            // recurse until the stack overflows.
            if visited_redirects.contains(&key) {
                return Err(ParseError::new(reader.cursor(), ParseErrorKind::RedirectCycle));
            }
            visited_redirects.push(key);
            let cursor = reader.cursor();
            let nodes_len = nodes.len();
            let arguments_len = arguments.len();
            self.parse_nodes(target, reader, visited_redirects, nodes, arguments, filter)?;

            // `/execute ... run` redirects into the ordinary root so built-in
            // commands retain their normal grammar.  Its own greedy fallback
            // is deliberately considered only when that root consumed
            // *nothing*: a known built-in root with bad arguments must remain
            // a built-in parse error, while an unknown terminal root may be
            // handed to the host dispatcher with the already-rewritten source.
            if reader.cursor() != cursor || self.node(child_id).children().is_empty() {
                return Ok(());
            }
            nodes.truncate(nodes_len);
            arguments.truncate(arguments_len);
            return self.parse_nodes(child_id, reader, visited_redirects, nodes, arguments, filter);
        }

        if !self.node(child_id).children().is_empty() {
            return self.parse_nodes(child_id, reader, visited_redirects, nodes, arguments, filter);
        }

        Ok(())
    }
}
