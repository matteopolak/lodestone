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
        let token_start = input.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let committed = &input[..token_start];
        let partial = &input[token_start..];
        let partial_lower = partial.to_lowercase();

        let node_id = self.walk_committed(committed);
        let node = self.node(node_id);

        let mut out: Vec<String> = Vec::new();
        for &child_id in node.children() {
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
    fn walk_committed(&self, committed: &str) -> NodeId {
        let mut reader = StringReader::new(committed);
        let mut node_id = self.root;

        while reader.can_read() {
            let node = self.node(node_id);
            let token = reader.peek_token();

            if let Some(&literal_child) = node.literal_children.get(token.as_str()) {
                reader.advance(token.chars().count());
                node_id = self.step_after_match(literal_child, &mut reader);
                continue;
            }

            let mut matched = None;
            for &child_id in &node.argument_children {
                let checkpoint = reader.cursor();
                let argument_type = self.node(child_id).argument_type().expect("argument_children only holds Argument nodes");
                if argument_type.parse(&mut reader).is_ok() {
                    matched = Some(child_id);
                    break;
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
