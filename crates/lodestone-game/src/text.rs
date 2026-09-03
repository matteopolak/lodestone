//! Chat-component interaction spans: the click/hover-carrying flatten a chat
//! hit-test needs.
//!
//! ## Why resolution comes first
//!
//! A server almost never sends prose. It sends *structure*: a death message is
//! `translate("death.attack.mob", [victim, killer])` where `killer` is itself
//! `translate("entity.minecraft.spider")`; a join line is
//! `translate("multiplayer.player.joined", [name])`; scoreboard titles, MOTDs,
//! sign text and item tooltips are all the same component model. The client is
//! what turns those keys into words, by looking them up in the language pack
//! (`en_us.json`). A surface that flattens the component without resolving it
//! shows the raw key on screen.
//!
//! [`lodestone_model::Text::resolve`] is where that happens, and its
//! [`lodestone_model::ResolvedText`] is the only thing the styled flatteners
//! accept — so the omission is a compile error rather than a defect that
//! reaches a pixel. The caller supplies the real table as a closure (an
//! `assets::Language` becomes one via `Language::translator`); this crate stays
//! version-free and asset-free.

use lodestone_model::Text;

// ---------------------------------------------------------------------------
// Interactive spans: `to_spans()` with `click`/`hover` kept.
// ---------------------------------------------------------------------------

/// One drawn run of text with its fully-inherited style and, when the
/// component tree set one, the `click`/`hover` interaction that applies to
/// the whole run.
///
/// [`lodestone_model::ResolvedText::to_spans`] cannot supply this: its
/// [`lodestone_model::text::TextSpan`] output carries only `text` and
/// `style`, so every render surface that flattens through it — which is all
/// of them, per that function's own "the one function a render surface
/// should call" doc — already has no `click`/`hover` left by the time it
/// reaches a draw call. `interactive_spans` walks the tree itself instead,
/// treating `click`/`hover` as inheritable exactly the way vanilla's own
/// style-apply step folds
/// `clickEvent`/`hoverEvent` into the same "child wins if set, else inherit
/// the parent's" rule as colour and every format flag. Nothing here is a
/// second decode — [`lodestone_model::Text::resolve`] already documents that
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
/// actually drew, exactly as [`crate::tablist::tab_list_view`] resolves before
/// it flattens), then lowered onto
/// [`lodestone_model::ResolvedText::to_interactive_spans`] — the model crate's
/// own `click`/`hover`-carrying sibling of `to_spans`, added independently for
/// the same gap this module first closed with a duplicate tree-walk of its
/// own. Delegating there rather than keeping that walk means there is exactly
/// one implementation of "how `click`/`hover` inherit down a `Text` tree",
/// owned by the crate that owns `Text` itself.
///
/// The `insertion` field `InteractiveTextSpan` carries is dropped at this
/// boundary — nothing under `crate::tablist`/`crate::chat` reads it yet; see
/// [`InteractiveSpan`]'s own doc if that changes.
#[must_use]
pub fn interactive_spans(
    text: &Text,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<InteractiveSpan> {
    text.resolve(translate)
        .to_interactive_spans()
        .into_iter()
        .map(|span| InteractiveSpan {
            text: span.text,
            style: span.style,
            click: span.click,
            hover: span.hover,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::{Text, TextColor, TextStyle};
    use lodestone_model::text::{ClickAction, ClickEvent, HoverAction, HoverEvent};

    /// A tiny table so tests do not depend on any real asset.
    fn tr(key: &str) -> Option<String> {
        let value = match key {
            "commands.seed.success" => "Seed: %s",
            _ => return None,
        };
        Some(value.to_owned())
    }

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
        let spans = msg.resolve(&no_tr).to_spans();
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
                font: None,
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
