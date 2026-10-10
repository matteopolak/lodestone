use super::*;

/// Padding, in logical-canvas pixels, around [`ChatHoverTooltip`]'s text.
pub(super) const CHAT_TOOLTIP_PAD: f32 = 3.0;
/// Max wrap width for [`ChatHoverTooltip`]'s body — vanilla's tooltip has
/// both an explicit per-component line break and its own wrap width
/// (the client tooltip component/Screen's get tooltip from item), and a hover body
/// that never breaks would run off the edge of the canvas for anything
/// longer than a couple of words.
pub(super) const CHAT_TOOLTIP_MAX_WIDTH: f32 = 200.0;

/// The tooltip body a hover event should paint, as styled runs with a literal
/// `\n` between lines — the shape [`ChatHoverTooltip::spans`] carries and
/// [`chat_hover_tooltip_layout`] splits on.
///
/// # Each action reaches an existing renderer rather than a new one
///
/// * `show_text` resolves its component and flattens it. Nothing else to do.
/// * `show_item` composes the *same* lines an inventory slot's tooltip shows,
///   through [`crate::container::tooltip::tooltip_lines`]. Writing a second
///   item-tooltip layout here would be free to disagree with the one a player
///   sees when they open their inventory — same stack, two different names.
///   `advanced` is the player's own advanced-tooltips option, threaded through
///   so the two surfaces answer that question the same way too.
/// * `show_entity` has three lines and they are all component-shaped: the
///   name when the payload carried one, the type through the entity type's own
///   description key, and the UUID as text. Composed here rather than in the
///   model crate because the type line needs the language table, which the
///   version-free, asset-free model deliberately cannot reach.
///
/// # Why a colour and not a style on the composed lines
///
/// [`crate::container::tooltip::TooltipLine`] carries either real
/// [`TextSpan`]s (when the line came from an authored component tree that
/// might hold a hex colour) or a flat RGBA it draws in. The flat ones convert
/// to [`lodestone_model::TextColor::Rgb`] rather than being matched back to a
/// named colour: the round trip through 8-bit channels is exact, and a new
/// tooltip colour needs no new arm here.
#[must_use]
pub fn hover_tooltip_spans(
    hover: &lodestone_model::text::HoverEvent,
    translate: &dyn Fn(&str) -> Option<String>,
    advanced: bool,
) -> Vec<TextSpan> {
    use lodestone_model::text::HoverEvent;

    match hover {
        HoverEvent::ShowText(value) | HoverEvent::Other { value, .. } => {
            value.resolve(translate).to_spans()
        }
        HoverEvent::ShowItem(stack) => {
            let stack = lodestone_game::item::ItemStack::from(stack.as_ref());
            join_tooltip_lines(&crate::container::tooltip::tooltip_lines(&stack, advanced))
        }
        HoverEvent::ShowEntity(entity) => {
            entity_tooltip_text(entity).resolve(translate).to_spans()
        }
    }
}

/// The three lines a `show_entity` tooltip shows, as one component tree with a
/// literal newline between them.
///
/// The order is the payload's own: the name first when there is one, then the
/// type, then the UUID. The type line is a translation with the type's
/// description as its single argument — two nested keys, both resolved by the
/// caller's table, so an unknown type shows the key rather than a fabricated
/// name. A payload with no readable type contributes no type line at all,
/// which is the honest answer: [`lodestone_model::text::HoverEntity::
/// type_translation_key`] returns `None` exactly then.
pub(super) fn entity_tooltip_text(entity: &lodestone_model::text::HoverEntity) -> lodestone_model::Text {
    use lodestone_model::Text;

    let mut lines: Vec<Text> = Vec::with_capacity(3);
    if let Some(name) = &entity.name {
        lines.push((**name).clone());
    }
    if let Some(key) = entity.type_translation_key() {
        lines.push(Text::translate(
            ENTITY_TOOLTIP_TYPE_KEY,
            vec![Text::translate(key, Vec::new())],
        ));
    }
    if let Some(uuid) = entity.uuid {
        lines.push(Text::literal(uuid.to_string()));
    }

    let mut root = Text::literal("");
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            root.extra.push(Text::literal("\n"));
        }
        root.extra.push(line);
    }
    root
}

/// The translation key wrapping a `show_entity` payload's type — `"Type: %s"`
/// in the English table, with the entity type's own description as the
/// argument.
pub(super) const ENTITY_TOOLTIP_TYPE_KEY: &str = "gui.entity_tooltip.type";

/// Flattens gathered tooltip lines into one span run with a literal `\n`
/// between lines. See [`hover_tooltip_spans`] for why a flat line's colour
/// becomes an explicit RGB style.
pub(super) fn join_tooltip_lines(lines: &[crate::container::tooltip::TooltipLine]) -> Vec<TextSpan> {
    let mut out: Vec<TextSpan> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(TextSpan { text: "\n".to_string(), style: TextStyle::default() });
        }
        match &line.spans {
            Some(spans) => out.extend(spans.iter().cloned()),
            None => out.push(TextSpan {
                text: line.text.clone(),
                style: TextStyle {
                    color: Some(lodestone_model::TextColor::Rgb(rgba_to_rgb(line.colour))),
                    ..TextStyle::default()
                },
            }),
        }
    }
    out
}

/// A 0..1 RGBA tooltip colour as the packed `0xrrggbb` a
/// [`lodestone_model::TextColor::Rgb`] holds. Alpha is dropped — the tooltip
/// box's own opacity is the background's, not the text's.
pub(super) fn rgba_to_rgb(colour: [f32; 4]) -> u32 {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (channel(colour[0]) << 16) | (channel(colour[1]) << 8) | channel(colour[2])
}

/// The pure half of [`draw_chat_hover_tooltip`]: wraps the tooltip body and
/// resolves its background rect in logical-canvas pixels (`x, y, w, h`),
/// taking the width-measuring/wrapping primitives as parameters exactly the
/// way [`wrap_legacy_with`] already does — so a gate can predict **the same
/// rect the draw fills** without a `Builder`, the same discipline
/// [`recipe_toast_rect`] established for the recipe toast.
///
/// Splits on a literal `\n` first — vanilla's `show_text` tooltips are
/// routinely multi-line — then greedily word-wraps each resulting line.
///
/// Takes [`TextSpan`]s, not a flat `&str`: [`ChatHoverTooltip::spans`] is the
/// span-carrying sibling of the flattened string this used to take, so a
/// hex-coloured `show_text` hover reaches real per-glyph colour rather than
/// losing it to [`lodestone_model::Text::to_legacy_string`]'s sixteen-code
/// ceiling.
pub(super) fn chat_hover_tooltip_layout(
    tooltip: &ChatHoverTooltip,
    canvas_w: f32,
    wrap: impl Fn(&[TextSpan], f32) -> Vec<Vec<TextSpan>>,
    width: impl Fn(&[TextSpan]) -> f32,
) -> (Vec<Vec<TextSpan>>, f32, f32, f32, f32) {
    const LINE_H: f32 = font::GLYPH_H as f32 + 1.0;

    let rows: Vec<Vec<TextSpan>> = split_span_paragraphs(tooltip.spans)
        .into_iter()
        .flat_map(|paragraph| wrap(&paragraph, CHAT_TOOLTIP_MAX_WIDTH))
        .collect();
    let tw = rows.iter().map(|row| width(row)).fold(0.0f32, f32::max);
    let th = rows.len() as f32 * LINE_H;
    let (mx, my) = tooltip.cursor;
    // `renderTooltip`'s own offset from the cursor, clamped onto the canvas —
    // the same expression `SuggestionLayer::Tooltip` above uses.
    let tx = (mx + 12.0).min((canvas_w - tw - CHAT_TOOLTIP_PAD * 2.0).max(0.0));
    let ty = (my - 12.0).max(0.0);
    (rows, tx, ty, tw + CHAT_TOOLTIP_PAD * 2.0, th + CHAT_TOOLTIP_PAD * 2.0)
}

/// Draws [`HudFrame::chat_hover_tooltip`], vanilla's `GuiGraphics.
/// renderTooltip` narrowed to placement and text — the same deliberate
/// cosmetic narrowing [`SuggestionLayer::Tooltip`] above already documents
/// for the identical shape (the tooltip render util's border gradient is not
/// modelled; this is a flat panel).
pub(super) fn draw_chat_hover_tooltip(b: &mut Builder, tooltip: &ChatHoverTooltip) {
    const LINE_H: f32 = font::GLYPH_H as f32 + 1.0;
    let (rows, tx, ty, w, h) = chat_hover_tooltip_layout(
        tooltip,
        b.w,
        |spans, max_w| b.wrap_spans(spans, max_w, 1.0),
        |spans| b.spans_width(spans, 1.0),
    );
    if rows.is_empty() {
        return;
    }
    b.rect_px(tx, ty, w, h, SUGGESTION_FILL);
    for (i, row) in rows.iter().enumerate() {
        b.text_spans(
            row,
            tx + CHAT_TOOLTIP_PAD,
            ty + CHAT_TOOLTIP_PAD + i as f32 * LINE_H,
            1.0,
            [1.0, 1.0, 1.0],
            1.0,
        );
    }
}

#[cfg(test)]
mod hover_payload_tests {
    use super::hover_tooltip_spans;
    use lodestone_model::Text;
    use lodestone_model::text::HoverEvent;

    /// The same jar-authored capture `lodestone_model::tests` reads — one
    /// fixture, one provenance. See
    /// `crates/lodestone-model/oracle-java/HoverEventOracle.java` for how it
    /// is produced and why it is captured rather than written by hand.
    const CAPTURE: &str =
        include_str!("../../../lodestone-model/tests/data/hover_events_26_2.json");

    /// The hover event carried by the captured component named `name`.
    fn captured_hover(name: &str) -> HoverEvent {
        let prefix = format!("{name}=");
        let json = CAPTURE
            .lines()
            .find_map(|l| l.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("the capture has no `{name}` line"));
        // Through the production flatten, not a private one: this is the same
        // call `HudRenderer::chat_interaction_at`'s input goes through.
        lodestone_game::text::interactive_spans(&Text::from_json(json), &|_| None)
            .into_iter()
            .find_map(|span| span.hover)
            .expect("the captured component carries a hover event")
    }

    /// The tooltip body as lines, undoing the `\n` joining
    /// [`hover_tooltip_spans`] does for the layout pass.
    fn lines(hover: &HoverEvent, advanced: bool) -> Vec<String> {
        let spans = hover_tooltip_spans(hover, &|_| None, advanced);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        joined.split('\n').map(str::to_owned).collect()
    }

    /// A `show_item` hover paints the item's own tooltip: the custom name and
    /// then the two authored lore lines, in that order. Every value comes
    /// from the capture, and the *composition* comes from the same
    /// line-gathering an inventory slot uses, so a player hovering the sword
    /// in chat and the sword in their inventory reads the same thing.
    #[test]
    fn a_captured_show_item_hover_paints_the_items_own_tooltip() {
        let hover = captured_hover("show_item");
        assert!(
            matches!(hover, HoverEvent::ShowItem(_)),
            "the capture must reach this as a typed item payload: {hover:?}"
        );
        assert_eq!(
            lines(&hover, false),
            ["Widowmaker", "Forged in the deep", "Bane of spiders"]
        );
    }

    /// With advanced tooltips on, the same hover grows the three lines the
    /// inventory tooltip grows: durability (the captured `1561` maximum less
    /// the captured `431` of damage), the item id, and the count of decoded
    /// components. Predicted from the capture's own numbers, not read back
    /// from the code.
    #[test]
    fn an_advanced_show_item_hover_adds_durability_id_and_component_count() {
        assert_eq!(
            lines(&captured_hover("show_item"), true),
            [
                "Widowmaker",
                "Forged in the deep",
                "Bane of spiders",
                "Durability: 1130 / 1561",
                "minecraft:diamond_sword",
                "4 Component(s)",
            ]
        );
    }

    /// A `show_entity` hover paints its three lines in the payload's own
    /// order: name, type, UUID. The type line goes through two nested
    /// translation keys, so a table that knows them produces words and one
    /// that does not falls back to the keys — checked both ways, because a
    /// key reaching the screen is the defect the resolve step exists to stop.
    #[test]
    fn a_captured_show_entity_hover_paints_name_type_and_uuid() {
        let hover = captured_hover("show_entity");
        assert!(
            matches!(hover, HoverEvent::ShowEntity(_)),
            "the capture must reach this as a typed entity payload: {hover:?}"
        );
        assert_eq!(
            lines(&hover, false),
            [
                "Boris",
                "gui.entity_tooltip.type",
                "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            ],
            "with no table the keys stand in for the words"
        );

        // The two strings the English table actually holds for those keys.
        let table = |key: &str| match key {
            "gui.entity_tooltip.type" => Some("Type: %s".to_owned()),
            "entity.minecraft.spider" => Some("Spider".to_owned()),
            _ => None,
        };
        let spans = hover_tooltip_spans(&hover, &table, false);
        let joined: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            joined.split('\n').collect::<Vec<_>>(),
            ["Boris", "Type: Spider", "6ba7b810-9dad-11d1-80b4-00c04fd430c8"]
        );
    }

    /// **The control.** A `show_text` hover is the one action whose payload a
    /// component flatten was always right for, and it still is — so a green
    /// item/entity result above cannot be explained by this function simply
    /// flattening everything it is given.
    #[test]
    fn a_show_text_hover_still_flattens_its_component_verbatim() {
        let hover = HoverEvent::ShowText(Box::new(Text::literal("plain body")));
        assert_eq!(lines(&hover, false), ["plain body"]);
    }
}

/// Gate for the chat `hover_event` tooltip (`docs/chat.md`'s
/// "Interactivity": the hit-test already found the hover, but nothing drew
/// it — the one link that stopped short of pixels).
///
/// Same instrument as `recipe_toast_gate`: a real triangle rasteriser over
/// NDC, not a vertex-sample count — `band_coverage`-shaped blindness (a quad
/// larger than the probe rect reads as zero) and the `opaque_ink`
/// clip-to-`i32` bug are both this repo's own measured false-negative
/// shapes for exactly this kind of gate, so this reuses the fixed
/// `coverage` helper rather than a new one.
#[cfg(test)]
mod chat_hover_tooltip_gate {
    use super::*;

    const W: u32 = 640;
    const H: u32 = 480;

    fn bare_frame<'a>(stats: &'a DebugStats) -> HudFrame<'a> {
        let mut f = HudFrame::new(stats);
        f.show_debug = false;
        f.crosshair = false;
        f
    }

    /// The no-font fallback path [`Builder::wrap_spans`]/
    /// [`Builder::spans_width`] themselves fall back to — `item_icon::text_w`
    /// summed over each span's plain text — reproduced with no `Builder` so
    /// the test can predict the draw's exact rect from outside it, the same
    /// discipline `recipe_toast_rect` established.
    fn no_font_spans_width(spans: &[TextSpan]) -> f32 {
        spans.iter().map(|s| item_icon::text_w(&s.text, 1.0)).sum()
    }

    fn no_font_spans_wrap(spans: &[TextSpan], max_w: f32) -> Vec<Vec<TextSpan>> {
        wrap_spans_with(no_font_spans_width, spans, max_w)
    }

    /// A single unstyled [`TextSpan`] carrying `s` — the plain-text fixture
    /// shape every test in this module needs now that
    /// [`ChatHoverTooltip::spans`] replaced the flattened `&str` it used to
    /// carry.
    fn plain_spans(s: &str) -> Vec<TextSpan> {
        vec![TextSpan { text: s.to_string(), style: TextStyle::default() }]
    }

    fn predicted_rect(tooltip: &ChatHoverTooltip, canvas_w: f32) -> (f32, f32, f32, f32) {
        let (_, x, y, w, h) =
            chat_hover_tooltip_layout(tooltip, canvas_w, no_font_spans_wrap, no_font_spans_width);
        (x, y, w, h)
    }

    fn rect_px_to_ndc(rect: (f32, f32, f32, f32), cw: f32, ch: f32) -> (f32, f32, f32, f32) {
        let (x, y, w, h) = rect;
        (
            2.0 * x / cw - 1.0,
            1.0 - 2.0 * (y + h) / ch,
            2.0 * (x + w) / cw - 1.0,
            1.0 - 2.0 * y / ch,
        )
    }

    /// **The control's premise, verified rather than assumed**: with no
    /// hover tooltip, nothing at all paints in the rect a tooltip at this
    /// cursor would fill.
    #[test]
    fn no_tooltip_frame_paints_nothing_in_the_tooltip_rect() {
        let stats = DebugStats::default();
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let spans = plain_spans("Nether Star");
        let tooltip = ChatHoverTooltip { spans: &spans, cursor: (100.0, 100.0) };
        let rect = rect_px_to_ndc(predicted_rect(&tooltip, cw), cw, ch);

        let geo = HudGeometry::build(&bare_frame(&stats), W, H);
        let (covered, inside, bbox) = coverage(&geo.verts, rect, 96);
        assert!(inside > 0, "the predicted tooltip rect must contain sample points");
        assert_eq!(
            covered, 0,
            "something other than the tooltip already paints its {inside}-cell \
             rect (bbox {bbox:?}) — the positive gate's premise is false"
        );
    }

    /// A hovered `hover_event` fills the predicted rect — the positive half,
    /// only meaningful because the control above just proved nothing else
    /// paints there.
    #[test]
    fn a_hover_tooltip_covers_its_own_predicted_rect() {
        let stats = DebugStats::default();
        let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let spans = plain_spans("Nether Star");
        let tooltip = ChatHoverTooltip { spans: &spans, cursor: (100.0, 100.0) };
        let rect = rect_px_to_ndc(predicted_rect(&tooltip, cw), cw, ch);

        let mut frame = bare_frame(&stats);
        frame.chat_hover_tooltip = Some(tooltip);
        let geo = HudGeometry::build(&frame, W, H);
        let (covered, inside, bbox) = coverage(&geo.verts, rect, 96);
        let fraction = covered as f32 / inside as f32;
        assert!(
            fraction > 0.9,
            "the tooltip background must fill its predicted rect: covered \
             {covered}/{inside} ({fraction:.3}) in rect {rect:?}, bbox {bbox:?}"
        );
    }

    /// Multi-line: a literal `\n` in the hover body (an enchanted book's
    /// enchantment list, vanilla-shaped) must grow the box taller than a
    /// single-line tooltip with the same widest line — the discriminating
    /// check that this draws `rows.len()` lines, not just the first one
    /// found and the rest silently dropped.
    #[test]
    fn a_multi_line_hover_body_is_taller_than_one_line() {
        let stats = DebugStats::default();
        let (cw, _) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let one_line_spans = plain_spans("Sharpness V");
        let one_line = ChatHoverTooltip { spans: &one_line_spans, cursor: (100.0, 100.0) };
        let two_lines_spans = plain_spans("Sharpness V\nCurse of Binding");
        let two_lines = ChatHoverTooltip { spans: &two_lines_spans, cursor: (100.0, 100.0) };

        let (_, _, h1, _) = predicted_rect(&one_line, cw);
        let (_, _, h2, _) = predicted_rect(&two_lines, cw);
        assert!(
            h2 > h1,
            "a two-line hover body ({h2}) must be taller than a one-line body ({h1})"
        );

        let mut frame = bare_frame(&stats);
        frame.chat_hover_tooltip = Some(two_lines.clone());
        let geo = HudGeometry::build(&frame, W, H);
        let (_, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, W, H);
        let rect = rect_px_to_ndc(predicted_rect(&two_lines, cw), cw, ch);
        let (covered, inside, bbox) = coverage(&geo.verts, rect, 96);
        let fraction = covered as f32 / inside as f32;
        assert!(
            fraction > 0.9,
            "the two-line tooltip must fill its own (taller) predicted rect: \
             covered {covered}/{inside} ({fraction:.3}) in rect {rect:?}, bbox {bbox:?}"
        );
    }

    /// The chain from `chat_interaction()` down to `HudFrame` -- proving the
    /// *producer*, not just that the draw paints when handed a frame by
    /// hand. `chat_interaction_at` (already exhaustively gated above by
    /// `chat_interaction_at_finds_a_hover_and_a_neighbouring_pixel_does_not`)
    /// is exercised directly here through `to_spans`, the exact conversion
    /// `app/redraw.rs`'s frame-building code applies before handing the
    /// text to `HudFrame::chat_hover_tooltip` — `to_legacy_string` sat here
    /// until fix routed this tooltip through spans instead, so
    /// a hex-coloured `show_text` hover keeps its real colour.
    #[test]
    fn a_hover_events_value_survives_to_spans_for_the_frame() {
        use lodestone_model::text::HoverEvent;

        let hover = HoverEvent::ShowText(Box::new(lodestone_model::Text::literal(
            "Diamond Sword",
        )));
        let spans = super::hover_tooltip_spans(&hover, &|_| None, false);
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            text, "Diamond Sword",
            "a plain-text hover_event's payload must survive verbatim to the \
             spans the tooltip frame field carries"
        );
    }

    /// A hex-coloured `show_text` hover must keep its real colour through to a
    /// drawn vertex, not flatten to the sixteen legacy codes
    /// `Text::to_legacy_string` is limited to. The fixture has three clauses
    /// (hex / inline `§` / named), with a control confirming that conversion
    /// through the legacy path loses the hex.
    #[test]
    fn a_hex_coloured_hover_tooltip_reaches_distinct_vertices_and_the_legacy_path_loses_it() {
        use lodestone_model::text::{Text, TextColor, TextContent, TextStyle};

        let hex = Text {
            content: TextContent::Literal("Hex".to_string()),
            style: TextStyle {
                font: None,
                color: Some(TextColor::Rgb(0x1a_2b3c)),
                ..TextStyle::default()
            },
            ..Text::default()
        };
        let inline_legacy = Text::literal("\u{00a7}cRed");
        let named = Text {
            content: TextContent::Literal("Gray".to_string()),
            style: TextStyle {
                font: None,
                color: Some(TextColor::Gray),
                ..TextStyle::default()
            },
            ..Text::default()
        };
        let root = Text {
            extra: vec![hex, inline_legacy, named],
            ..Text::default()
        };
        let spans = root.resolve(&|_| None).to_spans();
        assert_eq!(spans.len(), 3, "sanity: three runs in, three runs out — {spans:?}");

        let stats = DebugStats::default();
        let tooltip = ChatHoverTooltip { spans: &spans, cursor: (10.0, 10.0) };
        let mut frame = bare_frame(&stats);
        frame.chat_hover_tooltip = Some(tooltip);
        let geo = HudGeometry::build(&frame, W, H);

        let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        let has_colour = |verts: &[f32], rgb: (u8, u8, u8)| {
            verts
                .chunks_exact(FLOATS_PER_VERTEX)
                .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
        };
        let expected = [
            ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
            ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
            ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
        ];
        let missing: Vec<&str> = expected
            .iter()
            .filter(|(_, rgb)| !has_colour(&geo.verts, *rgb))
            .map(|(name, _)| *name)
            .collect();
        assert!(
            missing.is_empty(),
            "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
        );

        // Control: the same three-way name through the *old* path — a single
        // plain span carrying the flattened legacy string, which
        // `Text::to_legacy_string` has no `TextColor::Rgb` representation for.
        // Must show the loss, or the assertion above proves nothing about
        // which path actually carries the colour.
        let flattened = root.resolve(&|_| None).to_legacy_string();
        let legacy_spans = vec![TextSpan { text: flattened, style: TextStyle::default() }];
        let legacy_tooltip = ChatHoverTooltip { spans: &legacy_spans, cursor: (10.0, 10.0) };
        let mut legacy_frame = bare_frame(&stats);
        legacy_frame.chat_hover_tooltip = Some(legacy_tooltip);
        let legacy_geo = HudGeometry::build(&legacy_frame, W, H);
        assert!(
            !has_colour(&legacy_geo.verts, (0x1a, 0x2b, 0x3c)),
            "control failed: the legacy-string path was expected to lose the hex colour \
             (that is the bug), but it drew it anyway — this test's premise is wrong"
        );
    }
}
