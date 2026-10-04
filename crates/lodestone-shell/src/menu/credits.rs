//! The end-poem and credits roll: text loading, layout and scroll state.
//!
//! The text is never part of this repository. It is read at runtime from the
//! active resource pack stack (`texts/end.txt`, `texts/credits.json`,
//! `texts/postcredits.txt` under `assets/minecraft/`), the same way the panorama
//! and the font are, so a pack that overrides one of those files changes the
//! roll. When none of the three can be read the screen has nothing to show and
//! [`Credits::is_empty`] tells the app to go straight to the respawn.
//!
//! This module is pure state: [`Credits::advance`] moves the scroll by wall time
//! and [`Credits::visible`] reports which lines are on the canvas. The frame is
//! built from it by `render::credits_frame`.


/// Width of the text column the poem wraps to, in logical pixels.
pub const COLUMN_W: f32 = 256.0;
/// Distance between consecutive lines.
pub const LINE_H: f32 = 12.0;
/// Where the first line sits below the canvas bottom at scroll zero. The
/// original screen leaves room there for a logo that is not drawn here, so the
/// first line rises into view after the same pause.
pub const START_BELOW_CANVAS: f32 = 150.0;
/// Scroll speed of the roll, in logical pixels per game tick.
const SPEED_PER_TICK: f32 = 0.5;
const TICKS_PER_SECOND: f32 = 20.0;
/// Speed multiplier while the speed-up key is held.
const SPEEDUP: f32 = 5.0;
/// Extra multiplier for each Control key held on top of the speed-up key.
const SPEEDUP_PER_CONTROL: f32 = 15.0;
/// Indent in front of every credited name.
const NAME_INDENT: &str = "           ";
/// The section divider line.
const SECTION_RULE: &str = "============";
/// Marks where the poem hides a word behind scrambled glyphs; replaced with a
/// short run of obfuscated filler.
const OBFUSCATE_TOKEN: &str = "\u{a7}f\u{a7}k\u{a7}a\u{a7}b";
/// Seed of the filler-length generator, so the roll looks the same every run.
const FILLER_SEED: u64 = 8_124_371;

/// The three source texts, each absent when the pack stack does not carry it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreditsText {
    /// The poem.
    pub poem: Option<String>,
    /// The structured credits, a JSON array of sections.
    pub credits: Option<String>,
    /// The closing quotation shown after the credits.
    pub postcredits: Option<String>,
}

impl CreditsText {
    /// Reads the three texts from the active pack stack.
    #[must_use]
    pub fn load() -> Self {
        let Some(manager) = crate::resources::open_vanilla_pack_stack() else {
            return Self::default();
        };
        let read = |name: &str| {
            manager
                .read(&format!("assets/minecraft/texts/{name}"))
                .and_then(|bytes| String::from_utf8(bytes).ok())
        };
        Self {
            poem: read("end.txt"),
            credits: read("credits.json"),
            postcredits: read("postcredits.txt"),
        }
    }
}

/// A text measure matching what the screen will draw with: the proportional pack
/// font when one is loaded, the fixed-advance fallback otherwise.
#[must_use]
pub fn font_measure() -> Box<dyn Fn(&str) -> f32> {
    match crate::hud::vanilla_font::VanillaFont::shared() {
        Some(font) => Box::new(move |s| font.width(s, 1.0)),
        None => Box::new(|s| super::render::text_px(s, 1.0)),
    }
}

/// One laid-out line of the roll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditLine {
    /// The text, with legacy `§` colour codes left in for the font.
    pub text: String,
    /// Whether the line is centred on the column rather than left-aligned.
    pub centered: bool,
}

/// The keys that steer the scroll, sampled each frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CreditsInput {
    /// The speed-up key (Space).
    pub speedup: bool,
    /// How many Control keys are held; each adds to the speed-up.
    pub controls: u8,
    /// Scroll backwards (Up).
    pub reverse: bool,
}

/// The roll's scroll state and its lines.
#[derive(Debug, Clone, PartialEq)]
pub struct Credits {
    lines: Vec<CreditLine>,
    scroll: f32,
    height: f32,
    poem: bool,
}

impl Credits {
    /// Lays out the roll from `text`, substituting `player` into the poem.
    #[must_use]
    pub fn new(text: &CreditsText, player: &str, measure: &dyn Fn(&str) -> f32) -> Self {
        let mut lines = Vec::new();
        let mut filler = FILLER_SEED;
        if let Some(poem) = &text.poem {
            push_poem(&mut lines, poem, player, &mut filler, measure);
        }
        if let Some(credits) = &text.credits {
            push_credits(&mut lines, credits);
        }
        if let Some(post) = &text.postcredits {
            push_poem(&mut lines, post, player, &mut filler, measure);
        }
        Self { lines, scroll: 0.0, height: 240.0, poem: text.poem.is_some() }
    }

    /// Whether the roll opens with the end poem. A roll with a poem sits over the
    /// moving end-portal backdrop; a credits-only roll sits over the tiled
    /// background.
    #[must_use]
    pub fn has_poem(&self) -> bool {
        self.poem
    }

    /// Whether there is nothing to show.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The scroll position, in logical pixels.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// Moves the scroll by `seconds` of wall time on a canvas `height` tall.
    /// The scroll never goes above the start.
    pub fn advance(&mut self, seconds: f32, height: f32, input: CreditsInput) {
        self.height = height;
        let multiplier = if input.speedup {
            SPEEDUP + f32::from(input.controls) * SPEEDUP_PER_CONTROL
        } else {
            1.0
        };
        let direction = if input.reverse { -1.0 } else { 1.0 };
        let per_second = SPEED_PER_TICK * TICKS_PER_SECOND * multiplier * direction;
        self.scroll = (self.scroll + seconds * per_second).max(0.0);
    }

    /// Top edge of the logo, measured from the canvas top. It rises with the
    /// text, starting 50 px below the canvas.
    #[must_use]
    pub fn logo_y(&self) -> f32 {
        self.height + 50.0 - self.scroll
    }

    /// How far the tiled background has moved: half the text's scroll.
    #[must_use]
    pub fn background_scroll(&self) -> f32 {
        self.scroll * 0.5
    }

    /// Whether the roll has scrolled past its last line and off the canvas.
    #[must_use]
    pub fn finished(&self) -> bool {
        let total = self.lines.len() as f32 * LINE_H;
        self.scroll > total + self.height * 2.0 + 24.0
    }

    /// The lines on the canvas now, each with its top edge measured from the
    /// canvas top.
    pub fn visible(&self) -> impl Iterator<Item = (&CreditLine, f32)> {
        let height = self.height;
        self.lines.iter().enumerate().filter_map(move |(i, line)| {
            let y = height + START_BELOW_CANVAS + i as f32 * LINE_H - self.scroll;
            (!line.text.is_empty() && y + LINE_H + 8.0 > 0.0 && y < height).then_some((line, y))
        })
    }
}

/// A small deterministic generator, so the filler lengths do not depend on a
/// global random source.
fn next_random(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

fn push_empty(lines: &mut Vec<CreditLine>) {
    lines.push(CreditLine { text: String::new(), centered: false });
}

fn push_poem(lines: &mut Vec<CreditLine>, text: &str, player: &str, filler: &mut u64, measure: &dyn Fn(&str) -> f32) {
    for raw in text.lines() {
        let mut line = raw.replace("PLAYERNAME", player);
        while let Some(at) = line.find(OBFUSCATE_TOKEN) {
            let run = 3 + (next_random(filler) % 4) as usize;
            let before = &line[..at];
            let after = &line[at + OBFUSCATE_TOKEN.len()..];
            line = format!("{before}\u{a7}f\u{a7}k{}{after}", "X".repeat(run));
        }
        for wrapped in wrap(&line, COLUMN_W, measure) {
            lines.push(CreditLine { text: wrapped, centered: false });
        }
        push_empty(lines);
    }
    for _ in 0..8 {
        push_empty(lines);
    }
}

fn push_credits(lines: &mut Vec<CreditLine>, json: &str) {
    let Ok(serde_json::Value::Array(sections)) = serde_json::from_str::<serde_json::Value>(json) else {
        return;
    };
    let centered = |text: String| CreditLine { text, centered: true };
    let left = |text: String| CreditLine { text, centered: false };
    let text_of = |value: &serde_json::Value, key: &str| {
        value.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned()
    };
    for section in &sections {
        lines.push(centered(format!("\u{a7}f{SECTION_RULE}")));
        lines.push(centered(format!("\u{a7}e{}", text_of(section, "section"))));
        lines.push(centered(format!("\u{a7}f{SECTION_RULE}")));
        push_empty(lines);
        push_empty(lines);
        let disciplines = section.get("disciplines").and_then(serde_json::Value::as_array);
        for discipline in disciplines.into_iter().flatten() {
            let name = text_of(discipline, "discipline");
            if !name.is_empty() {
                lines.push(centered(format!("\u{a7}e{name}")));
                push_empty(lines);
                push_empty(lines);
            }
            let titles = discipline.get("titles").and_then(serde_json::Value::as_array);
            for title in titles.into_iter().flatten() {
                lines.push(left(format!("\u{a7}7{}", text_of(title, "title"))));
                let names = title.get("names").and_then(serde_json::Value::as_array);
                for name in names.into_iter().flatten().filter_map(serde_json::Value::as_str) {
                    lines.push(left(format!("\u{a7}f{NAME_INDENT}{name}")));
                }
                push_empty(lines);
                push_empty(lines);
            }
        }
    }
}

/// The legacy colour code in effect at the end of `text`, if any.
fn active_colour(text: &str) -> Option<char> {
    let mut colour = None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{a7}'
            && let Some(code) = chars.next()
        {
            match code.to_ascii_lowercase() {
                'r' => colour = None,
                '0'..='9' | 'a'..='f' => colour = Some(code),
                _ => {}
            }
        }
    }
    colour
}

/// Word-wraps `text` to `max_px`, carrying the colour in effect across the
/// break so a continuation line keeps its speaker's colour.
fn wrap(text: &str, max_px: f32, measure: &dyn Fn(&str) -> f32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        let candidate =
            if current.is_empty() { word.to_owned() } else { format!("{current} {word}") };
        if measure(&candidate) <= max_px || current.is_empty() {
            current = candidate;
            continue;
        }
        let carry = active_colour(&current);
        out.push(std::mem::take(&mut current));
        current = match carry {
            Some(code) => format!("\u{a7}{code}{word}"),
            None => word.to_owned(),
        };
    }
    out.push(current);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixed-advance measure: six pixels per visible character.
    fn fixed(s: &str) -> f32 {
        crate::menu::render::text_px(s, 1.0)
    }

    fn sample() -> CreditsText {
        CreditsText {
            poem: Some("\u{a7}3Hello PLAYERNAME.\n\u{a7}2Second.".to_owned()),
            credits: Some(
                r#"[{"section":"S","disciplines":[{"discipline":"D","titles":[{"title":"T","names":["N"]}]}]}]"#
                    .to_owned(),
            ),
            postcredits: Some("Fin".to_owned()),
        }
    }

    #[test]
    fn missing_texts_leave_nothing_to_show() {
        assert!(Credits::new(&CreditsText::default(), "x", &fixed).is_empty());
        assert!(!Credits::new(&sample(), "x", &fixed).is_empty());
    }

    #[test]
    fn the_poem_substitutes_the_player_and_keeps_the_speaker_colours() {
        let credits = Credits::new(&sample(), "Alex", &fixed);
        assert_eq!(credits.lines[0].text, "\u{a7}3Hello Alex.");
        assert_eq!(credits.lines[2].text, "\u{a7}2Second.");
        assert!(credits.lines[1].text.is_empty(), "a blank line follows each paragraph");
    }

    #[test]
    fn the_obfuscation_marker_becomes_a_short_run_of_scrambled_filler() {
        let text = CreditsText {
            poem: Some(format!("\u{a7}3before {OBFUSCATE_TOKEN}after")),
            ..CreditsText::default()
        };
        let line = &Credits::new(&text, "x", &fixed).lines[0].text;
        let body = line.strip_prefix("\u{a7}3before \u{a7}f\u{a7}k").unwrap();
        let filler = body.strip_suffix("after").unwrap();
        assert!((3..=6).contains(&filler.len()) && filler.chars().all(|c| c == 'X'), "{line:?}");
        assert!(!line.contains("\u{a7}a\u{a7}b"), "the marker is gone");
    }

    #[test]
    fn a_wrapped_poem_line_keeps_its_colour_on_the_continuation() {
        let long = format!("\u{a7}3{}", "word ".repeat(30));
        let wrapped = wrap(long.trim_end(), COLUMN_W, &fixed);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().all(|line| line.starts_with("\u{a7}3")), "{wrapped:?}");
        assert!(wrapped.iter().all(|line| fixed(line) <= COLUMN_W));
    }

    #[test]
    fn credits_sections_are_centred_headings_and_indented_names() {
        let text = CreditsText { credits: sample().credits, ..CreditsText::default() };
        let credits = Credits::new(&text, "x", &fixed);
        let centred: Vec<_> = credits.lines.iter().filter(|l| l.centered).map(|l| l.text.as_str()).collect();
        assert_eq!(centred, ["\u{a7}f============", "\u{a7}eS", "\u{a7}f============", "\u{a7}eD"]);
        assert!(credits.lines.iter().any(|l| !l.centered && l.text == format!("\u{a7}f{NAME_INDENT}N")));
    }

    #[test]
    fn the_roll_scrolls_ten_pixels_a_second_and_speeds_up_with_space_and_control() {
        let mut credits = Credits::new(&sample(), "x", &fixed);
        credits.advance(1.0, 240.0, CreditsInput::default());
        assert!((credits.scroll() - 10.0).abs() < 1e-4);
        credits.advance(1.0, 240.0, CreditsInput { speedup: true, ..Default::default() });
        assert!((credits.scroll() - 10.0 - 50.0).abs() < 1e-3, "space is five times faster");
        let before = credits.scroll();
        credits.advance(1.0, 240.0, CreditsInput { speedup: true, controls: 1, ..Default::default() });
        assert!((credits.scroll() - before - 200.0).abs() < 1e-3, "one control makes it twenty times");
        let before = credits.scroll();
        credits.advance(1.0, 240.0, CreditsInput { controls: 2, ..Default::default() });
        assert!((credits.scroll() - before - 10.0).abs() < 1e-4, "control alone changes nothing");
    }

    #[test]
    fn up_scrolls_backwards_and_stops_at_the_start() {
        let mut credits = Credits::new(&sample(), "x", &fixed);
        credits.advance(2.0, 240.0, CreditsInput::default());
        credits.advance(5.0, 240.0, CreditsInput { reverse: true, ..Default::default() });
        assert_eq!(credits.scroll(), 0.0);
    }

    #[test]
    fn the_roll_finishes_two_canvases_after_the_last_line() {
        let mut credits = Credits::new(&sample(), "x", &fixed);
        let total = credits.lines.len() as f32 * LINE_H;
        let end = total + 2.0 * 240.0 + 24.0;
        credits.advance(0.0, 240.0, CreditsInput::default());
        credits.scroll = end;
        assert!(!credits.finished(), "the threshold is exclusive");
        credits.scroll = end + 0.5;
        assert!(credits.finished());
    }

    #[test]
    fn only_lines_on_the_canvas_are_visible_and_they_rise_from_below() {
        let mut credits = Credits::new(&sample(), "x", &fixed);
        credits.advance(0.0, 240.0, CreditsInput::default());
        assert_eq!(credits.visible().count(), 0, "everything starts below the canvas");
        // Scroll until the first line is 100 px from the canvas top.
        credits.scroll = 240.0 + START_BELOW_CANVAS - 100.0;
        let (line, y) = credits.visible().next().unwrap();
        assert_eq!(line.text, "\u{a7}3Hello x.");
        assert!((y - 100.0).abs() < 1e-4, "{y}");
    }

    /// Against the real archive: the three texts load from the pack stack and lay
    /// out into a roll whose poem has the player's name in place of the marker.
    /// Skipped only when no archive is installed at all.
    #[test]
    fn only_a_roll_with_a_poem_asks_for_the_end_portal_backdrop() {
        let with = Credits::new(
            &CreditsText { poem: Some("a".to_owned()), ..CreditsText::default() },
            "x",
            &fixed,
        );
        let without = Credits::new(
            &CreditsText { credits: Some("[]".to_owned()), ..CreditsText::default() },
            "x",
            &fixed,
        );
        assert!(with.has_poem() && !without.has_poem());
    }

    #[test]
    fn the_installed_pack_supplies_all_three_texts() {
        let installed = lodestone_mc_cache::cache_root()
            .is_some_and(|dir| dir.join("lodestone-resources.zip").is_file());
        if !installed {
            return;
        }
        let text = CreditsText::load();
        assert!(text.poem.is_some() && text.credits.is_some() && text.postcredits.is_some(), "{text:?}");
        let measure = font_measure();
        let credits = Credits::new(&text, "Alexandra", &*measure);
        let poem_only = Credits::new(
            &CreditsText { poem: text.poem.clone(), ..CreditsText::default() },
            "Alexandra",
            &*measure,
        );
        assert!(
            poem_only.lines.iter().all(|l| measure(&l.text) <= COLUMN_W),
            "poem lines are wrapped to the {COLUMN_W} px column by the font that draws them"
        );
        assert!(credits.lines.iter().any(|l| l.text.contains("Alexandra")), "PLAYERNAME is substituted");
        assert!(!credits.lines.iter().any(|l| l.text.contains("PLAYERNAME")));
        assert!(!credits.lines.iter().any(|l| l.text.contains(OBFUSCATE_TOKEN)));
        assert!(credits.lines.iter().any(|l| l.centered), "credit sections are laid out");
        assert!(credits.lines.len() > 500, "the roll is long: {}", credits.lines.len());
    }
}
