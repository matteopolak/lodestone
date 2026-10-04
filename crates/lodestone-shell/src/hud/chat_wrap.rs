use super::*;

/// A chat line is fully lit for most of its life, then fades over its last
/// [`CHAT_FADE_SECS`] before disappearing at [`CHAT_VISIBLE_SECS`] — matching
/// vanilla's "recent messages fade out when the box is closed" behaviour. Only
/// used while the chat box is closed; open, every line is drawn at full alpha.
pub(super) fn chat_line_alpha(age: f32) -> f32 {
    const CHAT_VISIBLE_SECS: f32 = 10.0;
    const CHAT_FADE_SECS: f32 = 2.0;
    if age <= CHAT_VISIBLE_SECS - CHAT_FADE_SECS {
        1.0
    } else if age >= CHAT_VISIBLE_SECS {
        0.0
    } else {
        (CHAT_VISIBLE_SECS - age) / CHAT_FADE_SECS
    }
}

/// The visible characters of a legacy `§`-coded string: each `§`+selector pair
/// is dropped. Both text paths draw codes zero-width, so measuring the raw
/// string over-counts by two characters per code and pushes centred lines left.
pub(super) fn strip_legacy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\u{00a7}' {
            if chars.next().is_none() {
                break;
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Greedy word-wrap of a legacy `§`-coded line into rows that each fit
/// `max_width_px`, measured by calling `measure` on each candidate row.
/// [`Builder::wrap_legacy`] binds `measure` to real vanilla proportional
/// glyph advances (when a [`VanillaFont`] is attached) or the fixed 5×7
/// advance otherwise — this free function takes the measure as a parameter
/// precisely so its wrap *decisions* can be tested against a hand-specified
/// width table with no `Builder`, atlas, or jar involved.
///
/// Mirrors vanilla's own reflow in shape (`GuiMessage.splitLines`, invoked
/// from `ChatComponent.addMessageToDisplayQueue`, vanilla's own chat-component rendering):
/// break on a space when the next word would overflow, and hard-break a
/// single word that alone exceeds the width so nothing can escape the box. A
/// `§` colour/format code seen before a break is carried onto the
/// continuation line, because a code resets formatting to just itself
/// (`Text::from_legacy`'s legacy semantics) — tracking only the single most
/// recent one is therefore sufficient to keep the colour continuous across
/// the wrap.
///
/// Never returns an empty vector: an empty `s` yields one empty row, and a
/// `max_width_px <= 0.0` (or a line that already fits) is returned as a
/// single unwrapped row rather than looping forever trying to shrink it.
///
/// Builds every candidate row **in place** in one reusable buffer, pushing and
/// truncating rather than `format!`-ing a fresh `String` per word (and per
/// character on a hard break) — the wrap *decisions* are identical, only the
/// allocation count changes. `pending_code` is likewise the selector `char`
/// alone rather than an owned two-character `String`. Combined with
/// [`ChatWrapCache`], which persists the result so a frame with no new message
/// re-wraps nothing at all, this is half (a).
///
/// **Splits on a literal `\n` first**, the same precedent
/// [`chat_hover_tooltip_layout`]'s own `wrap` closure and
/// `menu::render::draw::wrap_measured` already establish: a chat component's
/// display text can carry one (a server sending a multi-line system message,
/// or a pasted multi-line player message), and before this the control
/// character reached the font as an ordinary — undrawable — glyph, drawing a
/// missing-glyph box instead of breaking the line. Delegated to
/// [`wrap_legacy_paragraph`] per paragraph rather than duplicated here,
/// matching `wrap_measured`'s own "one wrapper, one set of rules" reasoning
/// for the identical greedy algorithm. A blank paragraph still yields one row
/// for free: `wrap_legacy_paragraph("")` already returns a single empty row
/// (this function's own "never empty" guarantee, restated below), so two
/// adjacent `\n`s correctly draw as a blank line rather than collapsing.
pub(super) fn wrap_legacy_with(measure: impl Fn(&str) -> f32, s: &str, max_width_px: f32) -> Vec<String> {
    let mut rows = Vec::new();
    for paragraph in s.split('\n') {
        rows.extend(wrap_legacy_paragraph(&measure, paragraph, max_width_px));
    }
    rows
}

/// One `\n`-free line's worth of [`wrap_legacy_with`] — the greedy
/// break-on-space / hard-break-an-overlong-word body, unchanged from before
/// the `\n` split was added above it.
///
/// Never returns an empty vector: an empty `s` yields one empty row, and a
/// `max_width_px <= 0.0` (or a line that already fits) is returned as a
/// single unwrapped row rather than looping forever trying to shrink it.
pub(super) fn wrap_legacy_paragraph(
    measure: impl Fn(&str) -> f32,
    s: &str,
    max_width_px: f32,
) -> Vec<String> {
    if max_width_px <= 0.0 || measure(s) <= max_width_px {
        return vec![s.to_string()];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut pending_code: Option<char> = None;
    // Flush `current` as a finished row and re-seed the buffer with the colour
    // code in force, keeping `current`'s allocation across rows.
    let flush = |rows: &mut Vec<String>, current: &mut String, code: Option<char>| {
        rows.push(current.clone());
        current.clear();
        if let Some(c) = code {
            current.push('\u{00a7}');
            current.push(c);
        }
    };
    for word in s.split(' ') {
        // The last `§`+selector pair inside this word, if any — what a
        // continuation line started *after* this word must be seeded with to
        // keep reading the same colour.
        let mut word_pending = pending_code;
        let mut chars = word.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                if let Some(code) = chars.next() {
                    word_pending = Some(code);
                }
            }
        }

        let before_word = current.len();
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        if measure(&current) <= max_width_px {
            pending_code = word_pending;
            continue;
        }
        current.truncate(before_word);
        if !current.is_empty() {
            flush(&mut rows, &mut current, pending_code);
        }
        let seed_len = current.len();
        current.push_str(word);
        if measure(&current) > max_width_px {
            // The word alone overflows even a fresh line: hard-break it
            // character by character. `§`/selector characters are
            // zero-width, so they never trigger a break by themselves.
            current.truncate(seed_len);
            for ch in word.chars() {
                let was_empty = current.is_empty();
                current.push(ch);
                if !was_empty && measure(&current) > max_width_px {
                    current.pop();
                    flush(&mut rows, &mut current, pending_code);
                    current.push(ch);
                }
            }
        }
        pending_code = word_pending;
    }
    rows.push(current);
    rows
}

/// Persisted wrapped-row cache for the chat log. Each message is split into
/// wrapped rows once when it arrives rather than once per frame.
///
/// ## What it is
///
/// The wrap is a pure function of `(display text, chat box width, chat pose
/// scale, font)`. The text changes on a `SYSTEM_CHAT`/`PLAYER_CHAT` packet; the
/// width and scale change on a resize or an options edit. Nothing in that set
/// changes per frame, so without a cache the whole log would be re-wrapped on
/// every frame.
///
/// ## How to change it
///
/// The geometry key is the whole invalidation story: any *new* input the wrap
/// starts depending on must join `width`/`scale` here, or the cache will serve
/// a stale layout. The font is not keyed because a resource reload rebuilds the
/// owning `App`. Entries are `Rc<[String]>` so a hit is a refcount bump rather
/// than a per-row `String` clone; the map is cleared wholesale when it grows
/// past [`Self::MAX_ENTRIES`] rather than evicted by age, which is adequate for
/// a bounded chat log and keeps the type free of ordering state.
#[derive(Debug, Default)]
pub struct ChatWrapCache {
    pub(super) inner: std::cell::RefCell<ChatWrapInner>,
}

#[derive(Debug, Default)]
pub(super) struct ChatWrapInner {
    /// The geometry the cached rows were wrapped for. `None` before the first
    /// wrap; any mismatch clears `rows`.
    pub(super) geometry: Option<(u32, u32)>,
    pub(super) rows: std::collections::HashMap<String, std::rc::Rc<[String]>>,
}

impl ChatWrapCache {
    /// Cleared wholesale past this many distinct lines.
    pub(super) const MAX_ENTRIES: usize = 256;

    /// The wrapped rows for `text` at this geometry, computing them with `wrap`
    /// only on a miss.
    pub(super) fn rows(
        &self,
        text: &str,
        width_px: f32,
        scale: f32,
        wrap: impl FnOnce(&str) -> Vec<String>,
    ) -> std::rc::Rc<[String]> {
        // Bit patterns, not the floats: the key must be `Hash`/`Eq`, and an
        // exact-bits comparison is the right test here anyway — these are
        // recomputed from the same expressions every frame, so equal geometry
        // is bit-equal geometry.
        let geometry = (width_px.to_bits(), scale.to_bits());
        let mut inner = self.inner.borrow_mut();
        if inner.geometry != Some(geometry) {
            inner.geometry = Some(geometry);
            inner.rows.clear();
        }
        if let Some(hit) = inner.rows.get(text) {
            return std::rc::Rc::clone(hit);
        }
        if inner.rows.len() >= Self::MAX_ENTRIES {
            inner.rows.clear();
        }
        let rows: std::rc::Rc<[String]> = wrap(text).into();
        inner.rows.insert(text.to_string(), std::rc::Rc::clone(&rows));
        rows
    }
}

#[cfg(test)]
mod chat_wrap_cache_tests {
    use super::ChatWrapCache;

    /// The whole point of the cache: a repeat frame with the same line at the
    /// same geometry must not call the wrapper again, and a geometry change
    /// must. The counter is the assertion — a test that only compared the
    /// returned rows would pass with no cache at all.
    #[test]
    fn a_repeat_line_at_the_same_geometry_wraps_exactly_once() {
        let cache = ChatWrapCache::default();
        let wraps = std::cell::Cell::new(0usize);
        let wrap = |_: &str| {
            wraps.set(wraps.get() + 1);
            vec!["hello".to_string(), "world".to_string()]
        };

        let first = cache.rows("hello world", 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1, "the first call must wrap");
        let second = cache.rows("hello world", 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1, "the second call at the same geometry must not");
        assert_eq!(&*first, &*second);

        // A different line at the same geometry is a genuine miss.
        let _ = cache.rows("another line", 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 2);

        // A resize or a chat-scale change invalidates everything: the same
        // text must be re-wrapped at the new width.
        let _ = cache.rows("hello world", 120.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 3, "a width change must invalidate the cache");
        let _ = cache.rows("hello world", 120.0, 2.0, &wrap);
        assert_eq!(wraps.get(), 4, "a scale change must invalidate the cache");
    }
}

/// The [`TextSpan`] sibling of [`strip_legacy`]: vanilla's
/// Vanilla's own strip-formatting routine deletes every `§`+code pair wholesale —
/// colour and all five format flags — for `options.chat.color == false`
/// (its own strip-colour helper).
/// A span list is already past that decode, so the equivalent operation is
/// resetting every run's style to [`TextStyle::default`]: nothing to delete,
/// because there is no literal code left to find.
///
/// Deliberately does not merge the now-identically-styled runs back together.
/// [`Builder::wrap_spans`]/[`Builder::text_spans`] do not require merged runs
/// to wrap or draw correctly — only the strings and the (now-uniform) styles
/// — and merging would be pure allocation for a path that only exists while
/// the option is off.
pub(super) fn strip_style_spans(spans: &[TextSpan]) -> Vec<TextSpan> {
    spans
        .iter()
        .map(|s| TextSpan {
            text: s.text.clone(),
            style: TextStyle::default(),
        })
        .collect()
}

/// **Splits on a literal `\n` first**, the [`TextSpan`] sibling of
/// [`wrap_legacy_with`]'s own split — same reasoning, same "one wrapper, one
/// set of rules" delegation to a per-paragraph body
/// ([`wrap_spans_paragraph`]). [`split_span_paragraphs`] does the splitting
/// (a `\n` can sit mid-span, so this cannot be a plain `&str` split the way
/// the legacy sibling's can — it has to walk the flat `(char, style)` stream
/// and drop the `\n` while keeping every other character's style) and a blank
/// paragraph still yields one empty row for free, from
/// [`wrap_spans_paragraph`]'s own "never empty" guarantee below.
pub(crate) fn wrap_spans_with(
    measure: impl Fn(&[TextSpan]) -> f32,
    spans: &[TextSpan],
    max_width_px: f32,
) -> Vec<Vec<TextSpan>> {
    let paragraphs = split_span_paragraphs(spans);
    let mut rows = Vec::new();
    for paragraph in &paragraphs {
        rows.extend(wrap_spans_paragraph(&measure, paragraph, max_width_px));
    }
    rows
}

/// Splits `spans`' flat character stream on `\n`, dropping the newline itself
/// and re-merging each resulting run back into minimal [`TextSpan`]s via
/// [`merge_styled_chars`] — so a `\n` sitting in the middle of a styled run
/// (a coloured sender name immediately followed by `\n` in the body, say)
/// still splits cleanly without losing which half of the run belongs to which
/// paragraph. An empty `spans` yields one (empty) paragraph, matching
/// [`wrap_spans_paragraph`]'s own empty-input guarantee.
pub(super) fn split_span_paragraphs(spans: &[TextSpan]) -> Vec<Vec<TextSpan>> {
    let mut paragraphs: Vec<Vec<(char, TextStyle)>> = vec![Vec::new()];
    for span in spans {
        for ch in span.text.chars() {
            if ch == '\n' {
                paragraphs.push(Vec::new());
            } else {
                paragraphs.last_mut().expect("seeded above").push((ch, span.style));
            }
        }
    }
    paragraphs.iter().map(|p| merge_styled_chars(p)).collect()
}

/// One `\n`-free paragraph's worth of [`wrap_spans_with`] — the greedy
/// break-on-space / hard-break-an-overlong-word body, unchanged from before
/// the `\n` split was added above it.
///
/// Greedy word-wrap of a styled span list into rows that each fit
/// `max_width_px`, measured by calling `measure` on each candidate row's
/// spans. The [`TextSpan`] sibling of [`wrap_legacy_paragraph`]: same greedy
/// break-on-space / hard-break-an-overlong-word algorithm (mirroring
/// vanilla's `GuiMessage.splitLines`), generalised from "carry the single
/// most recent `§` code onto the continuation line" to "every character
/// keeps its own already-resolved style" — a styled chat line can carry more
/// than one colour change per wrapped row (a sender name in one colour
/// immediately followed by a hex-coloured body, say), which a
/// single-pending-code model cannot represent.
///
/// Works over a flat `(char, TextStyle)` stream built from `spans` rather
/// than over the spans themselves, which is what lets a word — or a wrap
/// point — fall in the middle of a style run: `spans` only promises that
/// each *run* has one style, not that a run boundary lines up with a space.
/// [`merge_styled_chars`] folds a finished row's stream back into minimal
/// [`TextSpan`] runs before it is measured or returned, so [`Builder::text_spans`]
/// still draws the fewest quads the styling actually requires.
///
/// Never returns an empty vector: an empty `spans` yields one empty row, and
/// a `max_width_px <= 0.0` (or a line that already fits) is returned as a
/// single unwrapped row rather than looping forever trying to shrink it —
/// exactly [`wrap_legacy_paragraph`]'s own guarantees, so [`ChatWrapCacheSpans`]
/// can share its caller's expectations about the shape of the result.
pub(super) fn wrap_spans_paragraph(
    measure: impl Fn(&[TextSpan]) -> f32,
    spans: &[TextSpan],
    max_width_px: f32,
) -> Vec<Vec<TextSpan>> {
    if spans.is_empty() {
        return vec![Vec::new()];
    }
    if max_width_px <= 0.0 || measure(spans) <= max_width_px {
        return vec![spans.to_vec()];
    }

    let chars: Vec<(char, TextStyle)> = spans
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.style)))
        .collect();

    let mut rows: Vec<Vec<TextSpan>> = Vec::new();
    let mut current: Vec<(char, TextStyle)> = Vec::new();
    let flush = |rows: &mut Vec<Vec<TextSpan>>, current: &mut Vec<(char, TextStyle)>| {
        rows.push(merge_styled_chars(current));
        current.clear();
    };

    // Walk word-by-word exactly as `wrap_legacy_with` walks `s.split(' ')`,
    // except a "word" here is a `(char, TextStyle)` slice rather than a `&str`
    // — the space between words carries the style of the character right
    // before it, matching how a bare space has no `§` code of its own to
    // carry either.
    let mut word_start = 0usize;
    for i in 0..=chars.len() {
        if i != chars.len() && chars[i].0 != ' ' {
            continue;
        }
        let word = &chars[word_start..i];
        word_start = i + 1;

        let before_word = current.len();
        if !current.is_empty() {
            let space_style = current.last().map_or_else(TextStyle::default, |&(_, s)| s);
            current.push((' ', space_style));
        }
        current.extend_from_slice(word);
        if measure(&merge_styled_chars(&current)) <= max_width_px {
            continue;
        }
        current.truncate(before_word);
        if !current.is_empty() {
            flush(&mut rows, &mut current);
        }
        current.extend_from_slice(word);
        if measure(&merge_styled_chars(&current)) > max_width_px {
            // The word alone overflows even a fresh line: hard-break it
            // character by character, exactly as `wrap_legacy_with` does.
            current.clear();
            for &(ch, style) in word {
                let was_empty = current.is_empty();
                current.push((ch, style));
                if !was_empty && measure(&merge_styled_chars(&current)) > max_width_px {
                    current.pop();
                    flush(&mut rows, &mut current);
                    current.push((ch, style));
                }
            }
        }
    }
    rows.push(merge_styled_chars(&current));
    rows
}

/// Folds a `(char, TextStyle)` stream into the fewest [`TextSpan`] runs that
/// still draw the same styling — consecutive characters sharing a style
/// collapse into one run. Shared by [`wrap_spans_with`]'s row-measuring and
/// row-flushing paths so a candidate row is measured with the exact spans it
/// will later be drawn with.
pub(super) fn merge_styled_chars(chars: &[(char, TextStyle)]) -> Vec<TextSpan> {
    let mut out: Vec<TextSpan> = Vec::new();
    for &(ch, style) in chars {
        match out.last_mut() {
            Some(last) if last.style == style => last.text.push(ch),
            _ => out.push(TextSpan {
                text: ch.to_string(),
                style,
            }),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Chat interactivity: hit-testing click_event/hover_event under the cursor.
// ---------------------------------------------------------------------------
//
// `lodestone_game::text::interactive_spans` is the data half — it keeps
// `click`/`hover` through the flatten `Text::to_spans()` cannot. This is the
// geometry half: wrapping those spans the same way the draw wraps plain
// `TextSpan`s ([`wrap_spans_with`]), and answering "which span is under this
// pixel" from the same free functions the draw itself calls
// (`chat_width_px`/`chat_height_px`/`chat_pose_scale`/`chat_line_h`/
// `chat_bottom`), for the same reason [`suggestion_layout`]'s own doc gives:
// a second, independently-derived copy of this geometry is exactly the
// failure mode this repo's evidence rules warn about.
//
// **Uncached, unlike the draw's own [`ChatWrapCacheSpans`].** A click or a
// hover is comparatively rare next to "every frame", so this re-wraps the
// visible window on every query rather than adding a second persisted cache
// keyed on the same geometry — a straightforward follow-up if hover ever
// needs to run every frame a tooltip is showing rather than only on
// `CursorMoved`.

/// Wraps [`lodestone_game::text::InteractiveSpan`]s the same word-break
/// algorithm [`wrap_spans_with`] applies to plain [`TextSpan`]s, extended to
/// carry each character's `click`/`hover` through the wrap. A message with no
/// interactivity at all wraps identically to before this function existed —
/// the whole-line fast path below measures the plain-style projection, same
/// as `wrap_spans_with`'s own.
pub(super) fn wrap_interactive_with(
    measure: impl Fn(&[TextSpan]) -> f32,
    spans: &[lodestone_game::text::InteractiveSpan],
    max_width_px: f32,
) -> Vec<Vec<lodestone_game::text::InteractiveSpan>> {
    use lodestone_game::text::InteractiveSpan;

    if spans.is_empty() {
        return vec![Vec::new()];
    }
    let as_text_spans: Vec<TextSpan> = spans
        .iter()
        .map(|s| TextSpan { text: s.text.clone(), style: s.style })
        .collect();
    if max_width_px <= 0.0 || measure(&as_text_spans) <= max_width_px {
        return vec![spans.to_vec()];
    }

    // The whole source span rides along with each character, rather than a
    // tuple of its interaction fields: every field of `InteractiveSpan` other
    // than `text` applies uniformly to the run, so carrying the run itself
    // means a new interaction field (as `insertion` was) needs no change to
    // the wrap at all.
    type Chr<'a> = (char, TextStyle, &'a InteractiveSpan);
    let chars: Vec<Chr<'_>> = spans
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.style, s)))
        .collect();

    let measure_current = |current: &[Chr<'_>]| -> f32 {
        let merged = merge_interactive_chars(current);
        let ts: Vec<TextSpan> =
            merged.iter().map(|s| TextSpan { text: s.text.clone(), style: s.style }).collect();
        measure(&ts)
    };
    let mut rows: Vec<Vec<InteractiveSpan>> = Vec::new();
    let mut current: Vec<Chr<'_>> = Vec::new();
    let flush = |rows: &mut Vec<Vec<InteractiveSpan>>, current: &mut Vec<Chr<'_>>| {
        rows.push(merge_interactive_chars(current));
        current.clear();
    };

    let mut word_start = 0usize;
    for i in 0..=chars.len() {
        if i != chars.len() && chars[i].0 != ' ' {
            continue;
        }
        let word = &chars[word_start..i];
        word_start = i + 1;

        let before_word = current.len();
        if !current.is_empty() {
            let &(_, style, source) = current.last().expect("just checked non-empty");
            current.push((' ', style, source));
        }
        current.extend_from_slice(word);
        if measure_current(&current) <= max_width_px {
            continue;
        }
        current.truncate(before_word);
        if !current.is_empty() {
            flush(&mut rows, &mut current);
        }
        current.extend_from_slice(word);
        if measure_current(&current) > max_width_px {
            current.clear();
            for &(ch, style, source) in word {
                let was_empty = current.is_empty();
                current.push((ch, style, source));
                if !was_empty && measure_current(&current) > max_width_px {
                    current.pop();
                    flush(&mut rows, &mut current);
                    current.push((ch, style, source));
                }
            }
        }
    }
    rows.push(merge_interactive_chars(&current));
    rows
}

/// [`merge_styled_chars`]'s sibling for the `(char, style, source run)` stream
/// [`wrap_interactive_with`] threads through — consecutive characters sharing
/// style **and** interaction collapse into one run, so a click boundary always
/// lands on a span boundary too.
pub(super) fn merge_interactive_chars(
    chars: &[(char, TextStyle, &lodestone_game::text::InteractiveSpan)],
) -> Vec<lodestone_game::text::InteractiveSpan> {
    use lodestone_game::text::InteractiveSpan;

    let mut out: Vec<InteractiveSpan> = Vec::new();
    for &(ch, style, source) in chars {
        let continues = |last: &InteractiveSpan| {
            last.style == style
                && last.click == source.click
                && last.hover == source.hover
                && last.insertion == source.insertion
        };
        match out.last_mut() {
            Some(last) if continues(last) => last.text.push(ch),
            _ => out.push(InteractiveSpan {
                text: ch.to_string(),
                style,
                click: source.click.clone(),
                hover: source.hover.clone(),
                insertion: source.insertion.clone(),
            }),
        }
    }
    out
}

/// The interactive span under `x` within one **already-wrapped** row —
/// measures the prefix up to and including each character with that
/// character's own run style, exactly the way the draw's glyph advance
/// would, so the boundary between two differently-styled runs is the same
/// pixel the draw would put it at.
pub(super) fn interactive_span_at<'a>(
    row: &'a [lodestone_game::text::InteractiveSpan],
    measure: impl Fn(&[TextSpan]) -> f32,
    x: f32,
) -> Option<&'a lodestone_game::text::InteractiveSpan> {
    if x < 0.0 {
        return None;
    }
    let mut prefix: Vec<TextSpan> = Vec::new();
    for span in row {
        for ch in span.text.chars() {
            let before = measure(&prefix);
            match prefix.last_mut() {
                Some(last) if last.style == span.style => last.text.push(ch),
                _ => prefix.push(TextSpan { text: ch.to_string(), style: span.style }),
            }
            let after = measure(&prefix);
            if x >= before && x < after {
                return Some(span);
            }
        }
    }
    None
}

/// The click/hover interaction under `(x, y)` (logical-canvas pixels) in the
/// chat scrollback, or `None` when nothing interactive is there — the
/// scrollback's own sibling of [`suggestion_layout`]'s popup hit-test.
///
/// `entries` is oldest-first, [`crate::sim::session::Sim::recent_chat_interactive`]'s
/// own shape; this walks it newest-first exactly as
/// [`HudGeometry::build_inner`]'s chat block does — same fade break, same
/// wrap, same row budget, same "a message's last wrapped row sits nearest the
/// bottom" stacking — because a hit-test built from a second copy of that
/// arithmetic is a hit-test that can drift from what the player actually
/// sees.
#[must_use]
pub fn chat_interaction_at(
    entries: &[(Vec<lodestone_game::text::InteractiveSpan>, f32)],
    canvas_w: f32,
    canvas_h: f32,
    opts: ChatDisplayOptions,
    chat_open: bool,
    measure: impl Fn(&[TextSpan]) -> f32,
    x: f32,
    y: f32,
) -> Option<lodestone_game::text::InteractiveSpan> {
    chat_interaction_at_scrolled(entries, canvas_w, canvas_h, opts, chat_open, 0, measure, x, y)
}

/// [`chat_interaction_at`] with the open chat screen's entry-based scroll
/// offset. `scrolled` is `ChatComponent.chatScrollbarPos`: entries at the live
/// bottom that are outside the rendered window must not be hit-testable.
#[must_use]
pub fn chat_interaction_at_scrolled(
    entries: &[(Vec<lodestone_game::text::InteractiveSpan>, f32)],
    canvas_w: f32,
    canvas_h: f32,
    opts: ChatDisplayOptions,
    chat_open: bool,
    scrolled: usize,
    measure: impl Fn(&[TextSpan]) -> f32,
    x: f32,
    y: f32,
) -> Option<lodestone_game::text::InteractiveSpan> {
    // `margin` is the *vertical* top clip only, matching the draw's own
    // `if y < margin` break. The horizontal origin is the chat column's own
    // inset — a different quantity, and sharing one name for both is how the
    // two came to be the same number in the first place.
    let margin = HUD_MARGIN;
    let pose_scale = chat_pose_scale(opts);
    let inset = CHAT_TEXT_INSET * pose_scale;
    let line_h = chat_line_h(opts, pose_scale);
    let box_w = chat_width_px(opts.width_pct.clamp(0.0, 1.0)).min(canvas_w);
    let height_pct = if chat_open { opts.height_pct_focused } else { opts.height_pct_unfocused };
    let box_h = chat_height_px(height_pct.clamp(0.0, 1.0));
    let bottom = chat_bottom(canvas_h, pose_scale);
    let max_visual_rows = (box_h / line_h).floor().max(1.0) as usize;
    let window_end = entries.len().saturating_sub(scrolled);
    let window_start = window_end.saturating_sub(max_visual_rows);
    let entries = &entries[window_start..window_end];

    // The text column runs from `inset` to `inset + box_w`; the old bound
    // stopped at `box_w`, so the last `inset` pixels of every wrapped line were
    // dead to hover and to clicks.
    if x < inset || x >= inset + box_w || line_h <= 0.0 {
        return None;
    }

    let mut row_i = 0usize;
    for (spans, age) in entries.iter().rev() {
        let alpha = if chat_open { 1.0 } else { chat_line_alpha(*age) };
        if alpha <= 0.0 {
            break;
        }
        let sub_rows = wrap_interactive_with(&measure, spans, box_w);
        for sub in sub_rows.iter().rev() {
            if row_i >= max_visual_rows {
                return None;
            }
            let row_y = bottom - (row_i as f32 + 1.0) * line_h;
            if row_y < margin {
                return None;
            }
            if y >= row_y - 1.0 && y < row_y - 1.0 + line_h {
                return interactive_span_at(sub, &measure, x - inset).cloned();
            }
            row_i += 1;
        }
    }
    None
}

/// The [`Self::chat_spans`](HudFrame::chat_spans) sibling of [`ChatWrapCache`]:
/// same persisted-wrap-result shape (a pure function of `(line, chat box
/// width, chat pose scale, font)`, recomputed only on a miss), keyed on
/// `Vec<TextSpan>` instead of `String` because a hex-coloured line has no
/// `§`-coded string to key on in the first place. `TextSpan`'s `Hash` (added
/// alongside its existing `Eq` in `lodestone-model`) is what makes that key
/// usable in a [`std::collections::HashMap`] at all.
#[derive(Debug, Default)]
pub struct ChatWrapCacheSpans {
    pub(super) inner: std::cell::RefCell<ChatWrapInnerSpans>,
}

#[derive(Debug, Default)]
pub(super) struct ChatWrapInnerSpans {
    pub(super) geometry: Option<(u32, u32)>,
    pub(super) rows: std::collections::HashMap<Vec<TextSpan>, std::rc::Rc<[Vec<TextSpan>]>>,
}

impl ChatWrapCacheSpans {
    /// Cleared wholesale past this many distinct lines — same bound and same
    /// reasoning as [`ChatWrapCache::MAX_ENTRIES`].
    pub(super) const MAX_ENTRIES: usize = 256;

    /// The wrapped rows for `spans` at this geometry, computing them with
    /// `wrap` only on a miss.
    pub(super) fn rows(
        &self,
        spans: &[TextSpan],
        width_px: f32,
        scale: f32,
        wrap: impl FnOnce(&[TextSpan]) -> Vec<Vec<TextSpan>>,
    ) -> std::rc::Rc<[Vec<TextSpan>]> {
        let geometry = (width_px.to_bits(), scale.to_bits());
        let mut inner = self.inner.borrow_mut();
        if inner.geometry != Some(geometry) {
            inner.geometry = Some(geometry);
            inner.rows.clear();
        }
        if let Some(hit) = inner.rows.get(spans) {
            return std::rc::Rc::clone(hit);
        }
        if inner.rows.len() >= Self::MAX_ENTRIES {
            inner.rows.clear();
        }
        let rows: std::rc::Rc<[Vec<TextSpan>]> = wrap(spans).into();
        inner.rows.insert(spans.to_vec(), std::rc::Rc::clone(&rows));
        rows
    }
}

#[cfg(test)]
mod chat_interaction_tests {
    use super::{
        TextSpan, TextStyle, chat_interaction_at, chat_interaction_at_scrolled,
        interactive_span_at, wrap_interactive_with,
    };
    use lodestone_game::text::InteractiveSpan;
    use lodestone_model::text::{ClickAction, ClickEvent, HoverEvent};

    fn plain(text: &str) -> InteractiveSpan {
        InteractiveSpan {
            text: text.to_string(),
            style: TextStyle::default(),
            click: None,
            hover: None,
            insertion: None,
        }
    }

    fn clickable(text: &str, url: &str) -> InteractiveSpan {
        InteractiveSpan {
            click: Some(ClickEvent { action: ClickAction::OpenUrl, value: url.to_string() }),
            ..plain(text)
        }
    }

    /// 6px per glyph, deterministic and non-trivial — the same shape as
    /// `wrap_spans_breaks_by_real_width_and_keeps_style_across_the_break`'s
    /// own fixed per-char table, so a wrap/hit-test bug shows up the same way
    /// a character-count fallback would: a wrong-by-a-fixed-ratio answer.
    fn measure(spans: &[TextSpan]) -> f32 {
        spans.iter().flat_map(|s| s.text.chars()).count() as f32 * 6.0
    }

    #[test]
    fn a_run_with_no_click_or_hover_wraps_unchanged() {
        let spans = [plain("hello world")];
        let rows = wrap_interactive_with(measure, &spans, 1000.0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], spans);
    }

    /// The word-break point is identical to [`wrap_spans_with`]'s own gate
    /// (`wrap_spans_breaks_by_real_width_and_keeps_style_across_the_break`),
    /// and the click on the second run survives onto its own wrapped row.
    #[test]
    fn wrap_keeps_click_on_the_run_it_belongs_to_across_a_break() {
        let spans = [plain("plain "), clickable("linktext", "https://example.invalid/")];
        // "plain " is 6 glyphs (36px), "linktext" is 8 (48px, so it must fit
        // on its own row without hard-breaking); a width of 50px fits neither
        // "plain " nor "linktext" *together* (84px) but fits each alone, so
        // vanilla's own word-wrap rule (never split a word that fits on a
        // fresh line) pushes the whole clickable word to row 2 intact.
        let rows = wrap_interactive_with(measure, &spans, 50.0);
        assert_eq!(rows.len(), 2, "rows: {rows:?}");
        let row0_text: String = rows[0].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(row0_text, "plain");
        assert!(rows[0].iter().all(|s| s.click.is_none()));
        let row1_text: String = rows[1].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(row1_text, "linktext");
        assert!(
            rows[1].iter().all(|s| s.click.is_some()),
            "the clickable word must carry its click onto the row it wrapped to: {:?}",
            rows[1]
        );
    }

    #[test]
    fn span_at_finds_the_run_under_the_pixel_and_none_past_the_end() {
        let row = [plain("abc"), clickable("def", "https://example.invalid/")];
        // "abc" occupies [0, 18); "def" occupies [18, 36).
        assert_eq!(interactive_span_at(&row, measure, 0.0), Some(&row[0]));
        assert_eq!(interactive_span_at(&row, measure, 17.9), Some(&row[0]));
        assert_eq!(interactive_span_at(&row, measure, 18.0), Some(&row[1]));
        assert_eq!(interactive_span_at(&row, measure, 35.9), Some(&row[1]));
        assert_eq!(
            interactive_span_at(&row, measure, 36.0),
            None,
            "past the last glyph must miss, not fall through to the last span"
        );
        assert_eq!(interactive_span_at(&row, measure, -1.0), None);
    }

    fn opts() -> super::ChatDisplayOptions {
        super::ChatDisplayOptions {
            scale: 1.0,
            width_pct: 1.0,
            height_pct_unfocused: 1.0,
            height_pct_focused: 1.0,
            line_spacing: 1.0,
            text_opacity: 1.0,
            background_opacity: 1.0,
            colors: true,
        }
    }

    /// End-to-end: a click on part of the newest (bottom-most) message is
    /// found under the pixel it actually occupies, missed one row up, and
    /// missed entirely outside the box — the three cases a geometry bug
    /// (wrong `chat_bottom`, wrong row stacking order, wrong x origin) would
    /// each get wrong in a different direction.
    #[test]
    fn chat_interaction_at_finds_a_click_where_the_row_actually_is() {
        let entries = vec![
            (vec![plain("older line, no link here")], 5.0),
            (vec![clickable("newest", "https://example.invalid/n")], 0.0),
        ];
        let canvas_w = 400.0;
        let canvas_h = 200.0;
        let o = opts();
        let pose_scale = super::chat_pose_scale(o);
        let line_h = super::chat_line_h(o, pose_scale);
        let bottom = super::chat_bottom(canvas_h, pose_scale);
        // The chat column's own text origin, derived exactly as the draw derives
        // it — not `HUD_MARGIN`, which is a different quantity these two tests
        // used to borrow and which no longer matches where a glyph lands.
        let inset = super::CHAT_TEXT_INSET * pose_scale;

        // Row 0 (newest, bottom-most) spans y in [bottom - line_h - 1, bottom - 1).
        let newest_row_y = bottom - line_h / 2.0;
        let hit = chat_interaction_at(
            &entries,
            canvas_w,
            canvas_h,
            o,
            true,
            measure,
            inset + 3.0,
            newest_row_y,
        );
        assert_eq!(
            hit.and_then(|s| s.click).map(|c| c.value),
            Some("https://example.invalid/n".to_string()),
            "the newest message's own click must be found on its own row"
        );

        // Row 1 (older line) sits one full line above — no click there.
        let older_row_y = newest_row_y - line_h;
        let miss = chat_interaction_at(
            &entries, canvas_w, canvas_h, o, true, measure, inset + 3.0, older_row_y,
        );
        assert_eq!(miss.and_then(|s| s.click), None, "the older line has no click at all");

        // Outside the box entirely (past the right edge) must miss too.
        let outside = chat_interaction_at(
            &entries, canvas_w, canvas_h, o, true, measure, canvas_w + 50.0, newest_row_y,
        );
        assert!(outside.is_none(), "a point outside the chat box must never hit");
    }

    /// The visible-hit half of chat command execution. The companion
    /// `app::tests::chat_click_dispatch::run_command_reaches_the_wire_as_a_real_command`
    /// consumes this exact event through the real outbound seam.
    #[test]
    fn a_visible_run_command_span_is_returned_for_dispatch() {
        let mut command = plain("run help");
        command.click = Some(ClickEvent {
            action: ClickAction::RunCommand,
            value: "/help".to_string(),
        });
        let canvas_w = 400.0;
        let canvas_h = 200.0;
        let o = opts();
        let pose = super::chat_pose_scale(o);
        let hit = chat_interaction_at(
            &[(vec![command], 0.0)],
            canvas_w,
            canvas_h,
            o,
            true,
            measure,
            super::CHAT_TEXT_INSET * pose + 1.0,
            super::chat_bottom(canvas_h, pose) - super::chat_line_h(o, pose) / 2.0,
        );
        assert_eq!(
            hit.and_then(|span| span.click),
            Some(ClickEvent { action: ClickAction::RunCommand, value: "/help".to_string() })
        );
    }

    /// The chat draw windows the oldest-first feed before it lays out rows.
    /// A scrollback hit-test must inspect that same window, rather than always
    /// treating the newest page as visible.
    #[test]
    fn chat_interaction_at_scrolled_hits_the_visible_older_page() {
        let canvas_w = 400.0;
        let canvas_h = 400.0;
        let o = opts();
        let pose_scale = super::chat_pose_scale(o);
        let line_h = super::chat_line_h(o, pose_scale);
        let bottom = super::chat_bottom(canvas_h, pose_scale);
        let rows = super::chat_lines_per_page(o, pose_scale, true);
        let mut entries: Vec<_> = (0..=rows)
            .map(|i| (vec![plain(&format!("line {i}"))], 0.0))
            .collect();
        entries[0] = (vec![clickable("older-link", "https://example.invalid/older")], 0.0);

        // With one entry scrolled off the live bottom, entry 0 is the
        // top-most visible row and entry `rows` is not drawn at all.
        let older_row_y = bottom - rows as f32 * line_h;
        let hit = chat_interaction_at_scrolled(
            &entries,
            canvas_w,
            canvas_h,
            o,
            true,
            1,
            measure,
            super::CHAT_TEXT_INSET * pose_scale + 1.0,
            older_row_y,
        );
        assert_eq!(
            hit.and_then(|span| span.click).map(|click| click.value),
            Some("https://example.invalid/older".to_string()),
            "the event must come from the scrolled page actually on screen"
        );
    }

    /// The hover half of the same shape, and the negative control that makes
    /// the positive result mean something: the same row, one pixel that is
    /// **not** the tooltip-bearing run, must come back `None`.
    #[test]
    fn chat_interaction_at_finds_a_hover_and_a_neighbouring_pixel_does_not() {
        let mut tipped = clickable("tip", "unused");
        tipped.click = None;
        tipped.hover = Some(HoverEvent::ShowText(Box::new(
            lodestone_model::Text::literal("tooltip body"),
        )));
        let entries = vec![(vec![plain("see "), tipped], 0.0)];
        let canvas_w = 400.0;
        let canvas_h = 200.0;
        let o = opts();
        let pose_scale = super::chat_pose_scale(o);
        let line_h = super::chat_line_h(o, pose_scale);
        let bottom = super::chat_bottom(canvas_h, pose_scale);
        // See the sibling test: the chat text origin, derived as the draw
        // derives it rather than borrowed from the HUD-wide margin.
        let inset = super::CHAT_TEXT_INSET * pose_scale;
        let row_y = bottom - line_h / 2.0;

        // "see " is 4 glyphs = 24px past the inset; "tip" starts there.
        let on_word = chat_interaction_at(
            &entries, canvas_w, canvas_h, o, true, measure, inset + 24.0 + 3.0, row_y,
        );
        assert!(
            on_word.is_some_and(|s| s.hover.is_some()),
            "the hover-bearing word must be found under its own pixel"
        );

        let before_word = chat_interaction_at(
            &entries, canvas_w, canvas_h, o, true, measure, inset + 3.0, row_y,
        );
        assert!(
            before_word.is_some_and(|s| s.hover.is_none()),
            "plain text one word earlier on the same row must not carry the hover"
        );
    }
}

#[cfg(test)]
mod chat_wrap_cache_spans_tests {
    use super::{ChatWrapCacheSpans, TextSpan};
    use lodestone_model::text::TextStyle;

    fn span(text: &str) -> TextSpan {
        TextSpan {
            text: text.to_string(),
            style: TextStyle::default(),
        }
    }

    /// The span-cache twin of `chat_wrap_cache_tests`'s own counter gate: a
    /// repeat frame at the same geometry must not re-wrap, and a geometry
    /// change must.
    #[test]
    fn a_repeat_line_at_the_same_geometry_wraps_exactly_once() {
        let cache = ChatWrapCacheSpans::default();
        let wraps = std::cell::Cell::new(0usize);
        let wrap = |_: &[TextSpan]| {
            wraps.set(wraps.get() + 1);
            vec![vec![span("hello")], vec![span("world")]]
        };

        let line = [span("hello"), span(" world")];
        let first = cache.rows(&line, 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1, "the first call must wrap");
        let second = cache.rows(&line, 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1, "the second call at the same geometry must not");
        assert_eq!(&*first, &*second);

        let other = [span("another line")];
        let _ = cache.rows(&other, 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 2, "different spans at the same geometry is a genuine miss");

        let _ = cache.rows(&line, 120.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 3, "a width change must invalidate the cache");
        let _ = cache.rows(&line, 120.0, 2.0, &wrap);
        assert_eq!(wraps.get(), 4, "a scale change must invalidate the cache");
    }

    /// The discriminating case a `String`-keyed cache cannot express: two
    /// entries with identical *text* but different *colour* must not collide
    /// — otherwise the second message would silently draw with the first
    /// one's wrap AND (downstream, in the draw itself) risk the wrong cached
    /// row shape if colour ever affected wrap width. Sharpened to actually
    /// affect width: one span is bold (`Font::advance_bold`'s `+1`/glyph),
    /// so a text-only key would serve the narrower non-bold wrap to the bold
    /// line too.
    #[test]
    fn same_text_different_style_is_not_a_cache_collision() {
        let cache = ChatWrapCacheSpans::default();
        let wraps = std::cell::Cell::new(0usize);
        let wrap = |s: &[TextSpan]| {
            wraps.set(wraps.get() + 1);
            vec![s.to_vec()]
        };

        let plain = [span("same text")];
        let mut bold_style = TextStyle::default();
        bold_style.bold = Some(true);
        let bold = [TextSpan {
            text: "same text".to_string(),
            style: bold_style,
        }];

        let _ = cache.rows(&plain, 80.0, 1.0, &wrap);
        assert_eq!(wraps.get(), 1);
        let _ = cache.rows(&bold, 80.0, 1.0, &wrap);
        assert_eq!(
            wraps.get(),
            2,
            "identical text with a different style must not reuse the plain entry's wrap"
        );
    }
}
