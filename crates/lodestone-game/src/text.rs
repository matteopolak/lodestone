//! Chat-component resolution: lowering a [`Text`] tree's `translate` nodes into
//! plain literals against a language table.
//!
//! ## Why this lives here and not in the renderer
//!
//! A server almost never sends prose. It sends *structure*: a death message is
//! `translate("death.attack.mob", [victim, killer])` where `killer` is itself
//! `translate("entity.minecraft.spider")`; a join line is
//! `translate("multiplayer.player.joined", [name])`; scoreboard titles, MOTDs,
//! sign text and item tooltips are all the same component model. The client is
//! what turns those keys into words, by looking them up in the language pack
//! (`en_us.json`). If the renderer prints the component without resolving it, the
//! screen shows `ENTITY.MINECRAFT.SPIDER` — the raw key — which is exactly the
//! bug this module fixes.
//!
//! [`lodestone_model::Text`] already knows how to *format* a translation
//! (argument substitution, `%s` / `%1$s`, `fallback`, style inheritance); what it
//! lacks is a way to apply a **custom** table while keeping styled output. Its
//! styled renderers ([`Text::to_spans`] / [`Text::to_legacy_string`]) are wired
//! to a tiny built-in stub table, so they cannot see `entity.minecraft.spider`.
//!
//! [`resolve`] closes that gap without reaching into the model: it walks the tree
//! and replaces every `translate` node with the equivalent *literal* subtree,
//! substituting the (recursively resolved) arguments as styled children. The
//! result contains no `translate` nodes at all, so rendering it with **any**
//! table — including the model's stub — produces the fully-resolved, correctly
//! styled line. The caller supplies the real table as a closure (an
//! `assets::Language` becomes one via `Language::translator`); this crate stays
//! version-free and asset-free.
//!
//! ## Fallback
//!
//! A missing key resolves to the component's `fallback` string if present, else
//! to the key itself — never to an error or empty text. Losing a translation
//! must never cost the message.
//!
//! ## What the model cannot represent
//!
//! `TextContent` models only `Literal` and `Translate`. `keybind` and `score`
//! components (and the `with`-less structural forms) have no representation and
//! are dropped to empty by the model's JSON/NBT front-ends before they ever reach
//! this crate, so there is nothing here to resolve for them. That is a model-layer
//! gap, noted so a future `keybind`/`score` arm knows where it belongs.

use lodestone_model::{Text, TextContent};

/// Recursion guard matching the model's own component depth limit.
const MAX_DEPTH: usize = 64;

/// Resolves every `translate` node in `text` against `translate`, returning an
/// equivalent tree that contains only literals.
///
/// - `text`, `translate` (with `%s` sequential and `%N$s` indexed arguments and
///   `%%` escapes), nested `extra`, and full style inheritance are all preserved:
///   each substituted argument and each `extra` child keeps its own style, which
///   inherits the resolving node's style exactly as vanilla does.
/// - A missing key falls back to the node's `fallback`, then to the key itself.
/// - Interactivity (`click`, `hover`, `insertion`) is carried through unchanged.
///
/// `translate` returns the format string for a key, or `None` to trigger the
/// fallback. An [`lodestone_assets::Language`] provides one via `Language::translator`.
#[must_use]
pub fn resolve(text: &Text, translate: &dyn Fn(&str) -> Option<String>) -> Text {
    resolve_node(text, translate, 0)
}

/// Convenience: resolve `text` and flatten it to a plain string. Equivalent to
/// `resolve(text, translate).to_plain_string()`, i.e. the fully-resolved wording
/// with styling discarded — the "what does this line say" form used by log lines
/// and by the live oracle.
#[must_use]
pub fn resolve_to_string(text: &Text, translate: &dyn Fn(&str) -> Option<String>) -> String {
    // Resolving to literals first means the trailing `to_plain_string` never
    // consults the model's stub table, so the custom table is authoritative.
    resolve(text, translate).to_plain_string()
}

fn resolve_node(node: &Text, translate: &dyn Fn(&str) -> Option<String>, depth: usize) -> Text {
    if depth > MAX_DEPTH {
        return Text::default();
    }

    // Resolve the node's own extra children up front; they render after this
    // node's content and inherit its style, unchanged by translation.
    let resolved_extra: Vec<Text> = node
        .extra
        .iter()
        .map(|child| resolve_node(child, translate, depth + 1))
        .collect();

    let mut out = Text {
        style: node.style,
        click: node.click.clone(),
        hover: node.hover.clone(),
        insertion: node.insertion.clone(),
        ..Text::default()
    };

    match &node.content {
        TextContent::Literal(literal) => {
            out.content = TextContent::Literal(literal.clone());
            out.extra = resolved_extra;
        }
        TextContent::Translate {
            key,
            with,
            fallback,
        } => {
            let pattern = translate(key)
                .or_else(|| fallback.clone())
                .unwrap_or_else(|| key.clone());
            let resolved_args: Vec<Text> = with
                .iter()
                .map(|arg| resolve_node(arg, translate, depth + 1))
                .collect();
            // Expand the pattern into this node's literal content plus a run of
            // children; the node's original `extra` follows those children.
            let mut pattern_children = Vec::new();
            expand_pattern(&pattern, &resolved_args, &mut out.content, &mut pattern_children);
            pattern_children.extend(resolved_extra);
            out.extra = pattern_children;
        }
    }

    out
}

/// Expands a translation format `pattern`, substituting `args`, into a leading
/// literal `content` plus a sequence of `children`.
///
/// The text before the first placeholder becomes `content`; every later literal
/// run becomes a plain-`Text` child, and every placeholder becomes the matching
/// resolved argument (itself styled). Supports `%s` (sequential), `%N$s`
/// (1-based indexed) and `%%` (literal `%`). An out-of-range or absent argument
/// contributes nothing, matching the model's formatter.
fn expand_pattern(
    pattern: &str,
    args: &[Text],
    content: &mut TextContent,
    children: &mut Vec<Text>,
) {
    let mut leading = String::new();
    let mut seen_placeholder = false;
    // Accumulates a literal run once we are past the leading segment.
    let mut buffer = String::new();

    let flush_run = |buffer: &mut String, children: &mut Vec<Text>| {
        if !buffer.is_empty() {
            children.push(Text::literal(std::mem::take(buffer)));
        }
    };

    let mut chars = pattern.chars().peekable();
    let mut next_auto = 0usize;
    while let Some(character) = chars.next() {
        if character != '%' {
            if seen_placeholder {
                buffer.push(character);
            } else {
                leading.push(character);
            }
            continue;
        }
        match chars.peek().copied() {
            Some('%') => {
                chars.next();
                if seen_placeholder {
                    buffer.push('%');
                } else {
                    leading.push('%');
                }
            }
            Some('s') => {
                chars.next();
                flush_run(&mut buffer, children);
                push_arg(args, next_auto, children);
                next_auto += 1;
                seen_placeholder = true;
            }
            Some(digit) if digit.is_ascii_digit() => {
                let mut index = 0usize;
                while let Some(d) = chars.peek().copied().filter(char::is_ascii_digit) {
                    chars.next();
                    index = index
                        .saturating_mul(10)
                        .saturating_add((d as usize) - ('0' as usize));
                }
                if chars.peek() == Some(&'$') {
                    chars.next();
                    if chars.peek() == Some(&'s') {
                        chars.next();
                    }
                }
                flush_run(&mut buffer, children);
                push_arg(args, index.saturating_sub(1), children);
                seen_placeholder = true;
            }
            _ => {
                if seen_placeholder {
                    buffer.push('%');
                } else {
                    leading.push('%');
                }
            }
        }
    }
    // Flush any trailing literal run that followed the last placeholder.
    flush_run(&mut buffer, children);

    *content = TextContent::Literal(leading);
}

fn push_arg(args: &[Text], index: usize, children: &mut Vec<Text>) {
    if let Some(arg) = args.get(index) {
        children.push(arg.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::{Text, TextColor, TextStyle};

    /// A tiny table so tests do not depend on any real asset.
    fn table(key: &str) -> Option<String> {
        let value = match key {
            "death.attack.mob" => "%1$s was slain by %2$s",
            "entity.minecraft.spider" => "Spider",
            "multiplayer.player.joined" => "%s joined the game",
            "chat.type.text" => "<%s> %s",
            "commands.seed.success" => "Seed: %s",
            _ => return None,
        };
        Some(value.to_owned())
    }

    fn tr(key: &str) -> Option<String> {
        table(key)
    }

    #[test]
    fn nested_translation_resolves_the_killer_name() {
        // death.attack.mob with the killer itself a translate node — the exact
        // shape of the ENTITY.MINECRAFT.SPIDER defect.
        let msg = Text::translate(
            "death.attack.mob",
            vec![
                Text::literal("Lodestone"),
                Text::translate("entity.minecraft.spider", vec![]),
            ],
        );
        assert_eq!(
            resolve_to_string(&msg, &tr),
            "Lodestone was slain by Spider"
        );
    }

    #[test]
    fn missing_key_falls_back_to_the_key_itself() {
        let msg = Text::translate("totally.unknown.key", vec![]);
        assert_eq!(resolve_to_string(&msg, &tr), "totally.unknown.key");
    }

    #[test]
    fn missing_key_prefers_the_components_fallback_string() {
        let msg = Text {
            content: TextContent::Translate {
                key: "unknown.key".to_string(),
                with: vec![Text::literal("X")],
                fallback: Some("fallback %s here".to_string()),
            },
            ..Text::default()
        };
        assert_eq!(resolve_to_string(&msg, &tr), "fallback X here");
    }

    #[test]
    fn sequential_and_indexed_placeholders_both_work() {
        let seq = Text::translate("chat.type.text", vec![Text::literal("bob"), Text::literal("hi")]);
        assert_eq!(resolve_to_string(&seq, &tr), "<bob> hi");

        let indexed = Text::translate(
            "death.attack.mob",
            vec![Text::literal("A"), Text::literal("B")],
        );
        assert_eq!(resolve_to_string(&indexed, &tr), "A was slain by B");
    }

    #[test]
    fn literal_percent_escape_is_preserved() {
        let msg = Text {
            content: TextContent::Translate {
                key: "unknown".to_string(),
                with: vec![],
                fallback: Some("100%% sure".to_string()),
            },
            ..Text::default()
        };
        assert_eq!(resolve_to_string(&msg, &tr), "100% sure");
    }

    #[test]
    fn trailing_and_leading_literals_around_placeholder() {
        let msg = Text::translate("commands.seed.success", vec![Text::literal("lodestone")]);
        assert_eq!(resolve_to_string(&msg, &tr), "Seed: lodestone");
    }

    #[test]
    fn resolved_tree_contains_no_translate_nodes() {
        let msg = Text::translate(
            "death.attack.mob",
            vec![
                Text::literal("A"),
                Text::translate("entity.minecraft.spider", vec![]),
            ],
        );
        let resolved = resolve(&msg, &tr);
        assert!(no_translate_nodes(&resolved), "resolution must lower every translate node");
    }

    fn no_translate_nodes(text: &Text) -> bool {
        matches!(text.content, TextContent::Literal(_))
            && text.extra.iter().all(no_translate_nodes)
    }

    #[test]
    fn style_inherits_down_a_nested_extra_chain() {
        // A red-bold root with a child that only sets italic: the child must end
        // up red + bold + italic (inherited colour and bold, own italic). This is
        // the part naive resolvers drop.
        let root = Text {
            content: TextContent::Literal("parent ".to_string()),
            style: TextStyle {
                color: Some(TextColor::Red),
                bold: Some(true),
                ..TextStyle::default()
            },
            extra: vec![Text {
                content: TextContent::Literal("child".to_string()),
                style: TextStyle {
                    italic: Some(true),
                    ..TextStyle::default()
                },
                ..Text::default()
            }],
            ..Text::default()
        };
        let resolved = resolve(&root, &tr);
        let spans = resolved.to_spans();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "parent ");
        assert_eq!(spans[0].style.color, Some(TextColor::Red));
        assert_eq!(spans[0].style.bold, Some(true));

        assert_eq!(spans[1].text, "child");
        // Inherited from the parent:
        assert_eq!(spans[1].style.color, Some(TextColor::Red));
        assert_eq!(spans[1].style.bold, Some(true));
        // The child's own attribute:
        assert_eq!(spans[1].style.italic, Some(true));
    }

    #[test]
    fn argument_keeps_its_own_style_and_inherits_the_translation_node_style() {
        // The translation node is gold; the victim argument is aqua. After
        // resolution the argument span must be aqua (its own colour wins) while a
        // plain literal chunk of the pattern stays gold (inherited).
        let msg = Text {
            content: TextContent::Translate {
                key: "death.attack.mob".to_string(),
                with: vec![
                    Text {
                        content: TextContent::Literal("Victim".to_string()),
                        style: TextStyle {
                            color: Some(TextColor::Aqua),
                            ..TextStyle::default()
                        },
                        ..Text::default()
                    },
                    Text::literal("Zombie"),
                ],
                fallback: None,
            },
            style: TextStyle {
                color: Some(TextColor::Gold),
                ..TextStyle::default()
            },
            ..Text::default()
        };
        let spans = resolve(&msg, &tr).to_spans();
        // Expect: [ "Victim"(aqua), " was slain by "(gold), "Zombie"(gold) ].
        let victim = spans.iter().find(|s| s.text == "Victim").expect("victim span");
        assert_eq!(victim.style.color, Some(TextColor::Aqua));
        let middle = spans
            .iter()
            .find(|s| s.text.contains("was slain by"))
            .expect("pattern literal span");
        assert_eq!(middle.style.color, Some(TextColor::Gold));
        let killer = spans.iter().find(|s| s.text == "Zombie").expect("killer span");
        assert_eq!(killer.style.color, Some(TextColor::Gold));
    }

    #[test]
    fn plain_literal_is_returned_unchanged() {
        let msg = Text::literal("just words");
        assert_eq!(resolve_to_string(&msg, &tr), "just words");
    }
}
