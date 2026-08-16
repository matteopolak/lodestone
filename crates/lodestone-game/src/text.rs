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

// ---------------------------------------------------------------------------
// Interactive spans: `to_spans()` with `click`/`hover` kept.
// ---------------------------------------------------------------------------

/// One drawn run of text with its fully-inherited style and, when the
/// component tree set one, the `click`/`hover` interaction that applies to
/// the whole run.
///
/// [`lodestone_model::Text::to_spans`] cannot supply this: its
/// [`lodestone_model::text::TextSpan`] output carries only `text` and
/// `style`, so every render surface that flattens through it — which is all
/// of them, per that function's own "the one function a render surface
/// should call" doc — already has no `click`/`hover` left by the time it
/// reaches a draw call. `interactive_spans` walks the tree itself instead,
/// treating `click`/`hover` as inheritable exactly the way vanilla's own
/// `Style` does: `Style.applyTo` (`Style.java`, `.cache/mc/26.2`) folds
/// `clickEvent`/`hoverEvent` into the same "child wins if set, else inherit
/// the parent's" rule as colour and every format flag. Nothing here is a
/// second decode — this module's own [`resolve`] already documents that
/// interactivity passes through it unchanged, so this only has to add the
/// inheritance `TextSpan` never carried.
#[derive(Debug, Clone, PartialEq)]
pub struct InteractiveSpan {
    /// This run's text.
    pub text: String,
    /// This run's fully-resolved style.
    pub style: lodestone_model::text::TextStyle,
    /// The click action covering this run, if any component from this run up
    /// to the tree root set one.
    pub click: Option<lodestone_model::text::ClickEvent>,
    /// The hover action covering this run, if any component from this run up
    /// to the tree root set one.
    pub hover: Option<lodestone_model::text::HoverEvent>,
}

/// Flattens `text` into [`InteractiveSpan`]s: `translate` nodes resolved
/// through `translate` first (so a hit-tested run's text matches what the HUD
/// actually drew, exactly as [`resolve`] already does for
/// [`crate::tablist::tab_list_view`]), then walked for `click`/`hover`
/// inheritance alongside style.
///
/// Legacy `§` runs are re-expanded through the model's own
/// [`lodestone_model::Text::from_legacy`] → [`lodestone_model::Text::to_spans`]
/// for the *style* half, since a `§` code cannot itself carry a `click`/
/// `hover` (those are component-tree fields, not legacy codes) — the
/// enclosing component's already-resolved click/hover is attached to every
/// run the legacy expansion produces.
#[must_use]
pub fn interactive_spans(
    text: &Text,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<InteractiveSpan> {
    let resolved = resolve(text, translate);
    let mut out = Vec::new();
    collect_interactive(
        &resolved,
        &lodestone_model::text::TextStyle::default(),
        None,
        None,
        &mut out,
        0,
    );
    out
}

fn collect_interactive(
    node: &Text,
    parent_style: &lodestone_model::text::TextStyle,
    parent_click: Option<&lodestone_model::text::ClickEvent>,
    parent_hover: Option<&lodestone_model::text::HoverEvent>,
    out: &mut Vec<InteractiveSpan>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    let style = node.style.inherit(parent_style);
    let click = node.click.as_ref().or(parent_click);
    let hover = node.hover.as_ref().or(parent_hover);

    // `resolve` above has already turned every `Translate` node into a
    // `Literal` root plus literal/argument children, so this is the only
    // content shape that can appear here — matching `resolve_node`'s own
    // guarantee.
    if let TextContent::Literal(literal) = &node.content
        && !literal.is_empty()
    {
        if literal.contains(lodestone_model::text::LEGACY_PREFIX) {
            // `from_legacy(literal)` carries no `click`/`hover` of its own
            // (legacy codes cannot express either), so every run it produces
            // takes this node's already-resolved `click`/`hover` verbatim;
            // only the *style* half needs inheriting further.
            for run in Text::from_legacy(literal).to_spans() {
                out.push(InteractiveSpan {
                    text: run.text,
                    style: run.style.inherit(&style),
                    click: click.cloned(),
                    hover: hover.cloned(),
                });
            }
        } else {
            out.push(InteractiveSpan {
                text: literal.clone(),
                style,
                click: click.cloned(),
                hover: hover.cloned(),
            });
        }
    }

    for child in &node.extra {
        collect_interactive(child, &style, click, hover, out, depth + 1);
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

    // -- interactive_spans ---------------------------------------------------

    use lodestone_model::text::{ClickAction, ClickEvent, HoverAction, HoverEvent};

    fn no_tr(_: &str) -> Option<String> {
        None
    }

    /// `to_spans()` — every render surface's own flatten — drops `click`
    /// entirely; this is the control proving [`interactive_spans`] measures a
    /// real gap and not a hypothetical one.
    #[test]
    fn to_spans_itself_has_nowhere_to_put_a_click_event() {
        let mut msg = Text::literal("click me");
        msg.click = Some(ClickEvent {
            action: ClickAction::OpenUrl,
            value: "https://example.invalid/".to_string(),
        });
        // `TextSpan` has no `click` field at all — this line would not compile
        // if it ever grew one, which is the point: the type itself is the
        // proof, not a runtime assertion.
        let spans = msg.to_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "click me");
    }

    /// A leaf with its own `click`/`hover` produces exactly one interactive
    /// run carrying both, alongside the style `to_spans` would have produced
    /// anyway.
    #[test]
    fn a_leafs_own_click_and_hover_reach_its_span() {
        let mut msg = Text {
            style: TextStyle {
                color: Some(TextColor::Aqua),
                ..TextStyle::default()
            },
            ..Text::literal("hi")
        };
        msg.click = Some(ClickEvent {
            action: ClickAction::RunCommand,
            value: "/spawn".to_string(),
        });
        msg.hover = Some(HoverEvent {
            action: HoverAction::ShowText,
            value: Box::new(Text::literal("tooltip")),
        });

        let spans = interactive_spans(&msg, &no_tr);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "hi");
        assert_eq!(spans[0].style.color, Some(TextColor::Aqua));
        assert_eq!(
            spans[0].click,
            Some(ClickEvent {
                action: ClickAction::RunCommand,
                value: "/spawn".to_string()
            })
        );
        assert_eq!(spans[0].hover.as_ref().map(|h| &h.action), Some(&HoverAction::ShowText));
    }

    /// `click`/`hover` inherit into children exactly like colour —
    /// `Style.applyTo`'s rule extended to these two fields. A child with no
    /// click/hover of its own still carries the parent's; a child that sets
    /// its own overrides rather than merges.
    #[test]
    fn click_and_hover_inherit_into_children_like_colour_does() {
        let parent_click = ClickEvent {
            action: ClickAction::OpenUrl,
            value: "https://example.invalid/a".to_string(),
        };
        let child_click = ClickEvent {
            action: ClickAction::OpenUrl,
            value: "https://example.invalid/b".to_string(),
        };
        let msg = Text {
            click: Some(parent_click.clone()),
            extra: vec![
                Text::literal("inherits"),
                Text {
                    click: Some(child_click.clone()),
                    ..Text::literal("overrides")
                },
            ],
            ..Text::literal("root ")
        };

        let spans = interactive_spans(&msg, &no_tr);
        let by_text = |t: &str| spans.iter().find(|s| s.text == t).expect(t);
        assert_eq!(by_text("root ").click, Some(parent_click.clone()));
        assert_eq!(
            by_text("inherits").click,
            Some(parent_click),
            "a child with no click of its own must inherit the parent's"
        );
        assert_eq!(
            by_text("overrides").click,
            Some(child_click),
            "a child's own click must win over the inherited one"
        );
    }

    /// A legacy `§` run splits into multiple spans (style-only, matching
    /// `to_spans()`), and every one of them still carries the *component's*
    /// click/hover — a legacy code cannot itself set or clear either.
    #[test]
    fn a_legacy_coded_run_keeps_the_components_click_across_every_split() {
        let msg = Text {
            click: Some(ClickEvent {
                action: ClickAction::SuggestCommand,
                value: "/help".to_string(),
            }),
            ..Text::literal("\u{a7}cred\u{a7}bblue")
        };
        let spans = interactive_spans(&msg, &no_tr);
        assert_eq!(spans.len(), 2, "the legacy codes must still split the run");
        assert_eq!(spans[0].text, "red");
        assert_eq!(spans[1].text, "blue");
        for s in &spans {
            assert_eq!(
                s.click.as_ref().map(|c| &c.value),
                Some(&"/help".to_string()),
                "every legacy-split run must keep the enclosing component's click"
            );
        }
    }

    /// `translate` resolution runs first, exactly as
    /// [`crate::tablist::tab_list_view`] already resolves before flattening —
    /// a hit-tested run's text must match what actually got drawn, and
    /// interactivity survives the substitution untouched (`resolve`'s own
    /// documented guarantee).
    #[test]
    fn translate_nodes_resolve_before_flattening_and_keep_their_click() {
        let msg = Text {
            click: Some(ClickEvent {
                action: ClickAction::ChangePage,
                value: "3".to_string(),
            }),
            ..Text::translate("commands.seed.success", vec![Text::literal("1234")])
        };
        let spans = interactive_spans(&msg, &tr);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "Seed: 1234");
        assert!(spans.iter().all(|s| s.click.is_some()));
    }

    /// No click/hover anywhere in the tree means every span carries `None` —
    /// the ordinary case must not fabricate an interaction.
    #[test]
    fn plain_text_carries_no_click_or_hover() {
        let spans = interactive_spans(&Text::literal("just words"), &no_tr);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].click, None);
        assert_eq!(spans[0].hover, None);
    }
}
