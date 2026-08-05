//! `CommandTree::suggest` — a restatement of
//! `CommandDispatcher::getCompletionSuggestions` + `Suggestions::merge`
//! (brigadier 1.3.10): walk to the node whose children are candidates for the
//! token currently being typed, collect every child's suggestions, filter by
//! the partial token (case-insensitively), then sort case-insensitively.
//!
//! Unlike [`crate::parse`], this never reports a hard error — an invalid
//! earlier token just stops the walk where it is and suggests from whatever
//! node was last successfully reached, matching vanilla's own tolerance (you
//! can still get suggestions after a typo elsewhere in the line, because
//! `getCompletionSuggestions` operates on the best-effort `ParseResults`
//! rather than requiring `execute`'s stricter success).

use crate::filter::{AllowAll, PermissionFilter};
use crate::node::{CommandTree, NodeId};
use crate::reader::StringReader;

impl CommandTree {
    /// Suggest completions for the token currently being typed at the end of
    /// `input`. Tokens before the last space are treated as "committed" and
    /// walked exactly as [`CommandTree::parse`] would (best-effort: a
    /// mismatch simply stops the walk rather than erroring); the text after
    /// the last space (or the whole input, if there is no space) is the
    /// partial token being completed against that node's children.
    ///
    /// Returned in the exact order vanilla's `Suggestions::merge` produces:
    /// deduplicated, sorted case-insensitively.
    pub fn suggest(&self, input: &str) -> Vec<String> {
        self.suggest_filtered(input, &AllowAll)
    }

    /// [`CommandTree::suggest`], with per-node permission gating (issue #122).
    ///
    /// A node the filter denies is **silently absent** from the results, and so
    /// is everything beneath it — the player is told nothing about a branch
    /// they cannot use, which is what vanilla achieves by never sending the
    /// node in the first place (see [`crate::filter`]). This is deliberately
    /// the opposite of [`CommandTree::parse_filtered`], which reports an
    /// explicit [`crate::ParseErrorKind::NoPermission`]: suggesting a gated
    /// node would leak the tree, while silently failing to *execute* one would
    /// be indistinguishable from a typo.
    pub fn suggest_filtered(&self, input: &str, filter: &dyn PermissionFilter) -> Vec<String> {
        let token_start = input.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let committed = &input[..token_start];
        let partial = &input[token_start..];
        let partial_lower = partial.to_lowercase();

        let node_id = self.walk_committed(committed, filter);
        let node = self.node(node_id);

        let mut out: Vec<String> = Vec::new();
        for &child_id in node.children() {
            // Pruned here as well as during the walk: the walk stops a player
            // descending *into* a denied branch, this stops the denied branch
            // being offered at the point it would be entered.
            if !self.node_allowed(child_id, filter) {
                continue;
            }
            let child = self.node(child_id);
            match child.as_literal() {
                Some(name) => {
                    if name.to_lowercase().starts_with(&partial_lower) {
                        out.push(name.to_string());
                    }
                }
                None => {
                    if let Some(argument_type) = child.argument_type() {
                        for candidate in argument_type.suggest(partial) {
                            if candidate.to_lowercase().starts_with(&partial_lower) {
                                out.push(candidate);
                            }
                        }
                    }
                }
            }
        }

        out.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        out.dedup();
        out
    }

    /// Best-effort walk of the fully-typed (space-terminated) prefix of the
    /// input, landing on the node whose children are the suggestion
    /// candidates. Deliberately does not share `parse_nodes`: it must never
    /// fail (a mismatch just means "stop here"), where `parse_nodes` must
    /// always propagate the specific error — the two failure policies don't
    /// compose cleanly into one function without a mode flag that would
    /// obscure both.
    fn walk_committed(&self, committed: &str, filter: &dyn PermissionFilter) -> NodeId {
        let mut reader = StringReader::new(committed);
        let mut node_id = self.root;

        while reader.can_read() {
            let node = self.node(node_id);
            let token = reader.peek_token();

            if let Some(&literal_child) = node.literal_children.get(token.as_str()) {
                if self.node_allowed(literal_child, filter) {
                    reader.advance(token.chars().count());
                    node_id = self.step_after_match(literal_child, &mut reader);
                    continue;
                }
                // Denied: stop the walk here rather than descending. Everything
                // beneath the denied node is thereby unreachable, which is the
                // subtree half of the pruning.
                break;
            }

            let mut matched = None;
            for &child_id in &node.argument_children {
                let checkpoint = reader.cursor();
                let argument_type = self.node(child_id).argument_type().expect("argument_children only holds Argument nodes");
                if argument_type.parse(&mut reader).is_ok() {
                    if self.node_allowed(child_id, filter) {
                        matched = Some(child_id);
                        break;
                    }
                    reader.set_cursor(checkpoint);
                    continue;
                }
                reader.set_cursor(checkpoint);
            }

            match matched {
                Some(child_id) => node_id = self.step_after_match(child_id, &mut reader),
                None => break,
            }
        }

        node_id
    }

    fn step_after_match(&self, child_id: NodeId, reader: &mut StringReader) -> NodeId {
        if reader.can_read() && reader.peek() == Some(' ') {
            reader.skip();
        }
        self.node(child_id).redirect().unwrap_or(child_id)
    }
}
