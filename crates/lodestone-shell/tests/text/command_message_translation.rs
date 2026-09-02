//! Vanilla's own command-feedback wording, resolved through this client's real
//! translate path.
//!
//! ## What this is for
//!
//! `/gamemode`, `/give` and `/gamerule` currently send **literal English
//! strings** from the server (`lodestone-server/src/commands/*.rs` call
//! `ctx.send_success(format!(…))`). Vanilla sends *components*:
//! `Component.translatable("commands.gamemode.success.self", mode)` where `mode`
//! is itself `Component.translatable("gameMode.creative")`. This file pins what
//! that component must resolve to, so the server-side change to emit real keys
//! has a target it can be checked against rather than a description.
//!
//! Everything the client needs already exists and is already wired — that was the
//! surprise here, and it is written down in `docs/message-translation.md`:
//!
//! * `resources.rs` loads `assets/minecraft/lang/en_us.json` out of the same
//!   `client.jar` every texture comes from (8,123 keys in 26.2),
//! * `Sim::translator()` hands it out as a `Fn(&str) -> Option<String>`,
//! * `lodestone_game::text::resolve` walks a `Text` tree replacing every
//!   `translate` node with its literal expansion — `%s`, `%N$s`, `%%`, nested
//!   arguments, `fallback`, key-as-last-resort, and full style inheritance,
//! * `hud::vanilla_font` really draws bold/italic/underline/strikethrough.
//!
//! ## Where the expected values come from
//!
//! Two independent outside sources, joined:
//!
//! | what | source |
//! |---|---|
//! | which key, and its argument **order** | the decompiled 26.2 command classes under `.cache/mc/26.2/src/net/minecraft/server/commands/`, cited per line below |
//! | the format string for each key | `assets/minecraft/lang/en_us.json` in `client.jar` |
//! | the **expected sentence** | those two, expanded **by hand** here |
//!
//! The hand expansion is the load-bearing part. Feeding the jar's pattern to the
//! resolver and then asserting the resolver agrees with the jar's pattern is
//! `decode(encode(x)) == x`; asserting it agrees with a sentence a human wrote out
//! from the record definition is not. `PATTERNS` below is therefore transcribed,
//! not read, and [`transcribed_patterns_match_the_real_en_us_json`] is the
//! `#[ignore]`d drift gate that catches a transcription going stale.

use lodestone_assets::Language;
use lodestone_model::{Text, TextColor, TextStyle};

/// The `en_us.json` entries these tests expand, transcribed from
/// `assets/minecraft/lang/en_us.json` in 26.2's `client.jar`.
///
/// Verified against the real file by
/// [`transcribed_patterns_match_the_real_en_us_json`], which is what stops this
/// list quietly becoming a table of this test's own opinions.
const PATTERNS: &[(&str, &str)] = &[
    // `GameModeCommand.java` / `:51`.
    ("commands.gamemode.success.self", "Set own game mode to %s"),
    (
        "commands.gamemode.success.other",
        "Set %s's game mode to %s",
    ),
    // `GameModeCommand.java` builds `"gameMode." + newType.getName()`.
    ("gameMode.creative", "Creative Mode"),
    ("gameMode.survival", "Survival Mode"),
    // `GameModeCommand.java`, the message the *target* receives.
    ("gameMode.changed", "Your game mode has been updated to %s"),
    // `GiveCommand.java` / `:102`.
    ("commands.give.success.single", "Gave %s %s to %s"),
    ("commands.give.success.multiple", "Gave %s %s to %s players"),
    // `GiveCommand.java`, a `sendFailure`.
    (
        "commands.give.failed.toomanyitems",
        "Can't give more than %s of %s",
    ),
    // `GameRuleCommand.java` / `:52`.
    ("commands.gamerule.set", "Gamerule %s is now set to: %s"),
    (
        "commands.gamerule.query",
        "Gamerule %s is currently set to: %s",
    ),
    // `CommandSourceStack.java`, the op broadcast — this is where vanilla's
    // *italics* come from.
    ("chat.type.admin", "[%s: %s]"),
    // An item display name, for `/give`'s second argument.
    ("item.minecraft.diamond_sword", "Diamond Sword"),
    // `Inventory.java`'s client-side default, already used by the container
    // screen — included as a cross-check that this list agrees with a key the
    // shell resolves in production.
    ("container.inventory", "Inventory"),
];

/// [`PATTERNS`] as the closure `lodestone_game::text::resolve` consumes — the
/// same shape `Sim::translator()` produces from the real table.
fn table() -> Language {
    let json: serde_json::Value = PATTERNS
        .iter()
        .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
        .collect::<serde_json::Map<_, _>>()
        .into();
    Language::from_json_bytes(json.to_string().as_bytes()).expect("a flat object of strings")
}

fn resolve(text: &Text) -> String {
    let lang = table();
    lodestone_game::text::resolve_to_string(text, &lang.translator())
}

/// `/gamemode creative` on yourself — `GameModeCommand.java`:
///
/// ```java
/// Component mode = Component.translatable("gameMode." + newType.getName());
/// source.sendSuccess(() -> Component.translatable("commands.gamemode.success.self", mode), true);
/// ```
///
/// The **nesting** is what makes this worth asserting rather than a string
/// compare: the argument is itself a translate component, so a resolver that
/// substituted arguments without recursing would produce "Set own game mode to
/// gameMode.creative" — plausible-looking, and wrong. The second assertion
/// computes that wrong hypothesis rather than describing it.
#[test]
fn gamemode_success_self_resolves_to_vanillas_sentence() {
    let mode = Text::translate("gameMode.creative", vec![]);
    let message = Text::translate("commands.gamemode.success.self", vec![mode]);

    // Hand-expanded from `"Set own game mode to %s"` + `"Creative Mode"`.
    assert_eq!(resolve(&message), "Set own game mode to Creative Mode");

    // The un-recursed hypothesis, computed from the same two constants.
    let unrecursed = format!(
        "{} {}",
        "Set own game mode to".to_owned(),
        "gameMode.creative"
    );
    assert_ne!(resolve(&message), unrecursed);

    // And the literal string the server sends *today*, which is the thing this
    // is meant to replace. Named so the difference is on the record: our current
    // wording drops the word "Mode".
    assert_ne!(resolve(&message), "Set own game mode to creative");
}

/// `/gamemode` on someone else — `GameModeCommand.java`,
/// `translatable("commands.gamemode.success.other", target.getDisplayName(), mode)`.
///
/// Two same-typed arguments in a pattern that reads naturally **either way
/// round**, which is exactly the case an order mistake survives review in: "Set
/// Steve's game mode to Creative Mode" and "Set Creative Mode's game mode to
/// Steve" are both grammatical. Both hypotheses are computed and the measurement
/// must land on one.
#[test]
fn gamemode_success_other_puts_the_player_first_and_the_mode_second() {
    let steve = Text::literal("Steve");
    let mode = Text::translate("gameMode.survival", vec![]);
    let right = Text::translate(
        "commands.gamemode.success.other",
        vec![steve.clone(), mode.clone()],
    );
    let swapped = Text::translate("commands.gamemode.success.other", vec![mode, steve]);

    assert_eq!(resolve(&right), "Set Steve's game mode to Survival Mode");
    assert_eq!(
        resolve(&swapped),
        "Set Survival Mode's game mode to Steve",
        "the control: the swapped order really does produce a different, equally \
         grammatical sentence, so the assertion above is not order-blind"
    );
}

/// `/give` — `GiveCommand.java`,
/// `translatable("commands.give.success.single", count, prototypeItemStack.getDisplayName(), <target>)`.
///
/// The middle argument is the item's **display name**, not its id. That is the
/// substantive difference from what the server sends today
/// (`format!("Gave {count} {} to {}", item.item, …)` interpolates
/// `minecraft:diamond_sword`), and it is asserted as a `assert_ne!` against the
/// id form so the distinction cannot be lost in a later edit.
#[test]
fn give_success_single_uses_the_items_display_name_not_its_id() {
    let message = Text::translate(
        "commands.give.success.single",
        vec![
            Text::literal("3"),
            Text::translate("item.minecraft.diamond_sword", vec![]),
            Text::literal("Steve"),
        ],
    );
    assert_eq!(resolve(&message), "Gave 3 Diamond Sword to Steve");
    assert_ne!(resolve(&message), "Gave 3 minecraft:diamond_sword to Steve");
}

/// `/gamerule` set and query — `GameRuleCommand.java` / `:52`. Two keys whose
/// *only* difference is "is now" versus "is currently", so a wrong key here is
/// invisible unless both are pinned.
#[test]
fn gamerule_set_and_query_are_two_different_sentences() {
    let args = || {
        vec![
            Text::literal("doDaylightCycle"),
            Text::literal("false"),
        ]
    };
    let set = Text::translate("commands.gamerule.set", args());
    let query = Text::translate("commands.gamerule.query", args());
    assert_eq!(
        resolve(&set),
        "Gamerule doDaylightCycle is now set to: false"
    );
    assert_eq!(
        resolve(&query),
        "Gamerule doDaylightCycle is currently set to: false"
    );
    assert_ne!(resolve(&set), resolve(&query));
}

/// Where the **italics** live: `CommandSourceStack.java`.
///
/// ```java
/// Component broadcast = Component.translatable("chat.type.admin", this.getDisplayName(), message)
///     .withStyle(ChatFormatting.GRAY, ChatFormatting.ITALIC);
/// ```
///
/// So a command's feedback is *not* italic to the caller — the op **broadcast**
/// is, and grey with it. Asserted on the resolved spans rather than on the tree,
/// because the point is that the nested feedback message **inherits** the
/// wrapper's grey italic: `TextStyle::inherit` is what makes that true, and a
/// resolver that reset style per substituted argument would leave the inner
/// sentence unstyled while the brackets stayed italic — which reads as "nearly
/// right" on screen.
#[test]
fn the_op_broadcast_is_grey_italic_and_the_nested_message_inherits_it() {
    let feedback = Text::translate(
        "commands.gamemode.success.self",
        vec![Text::translate("gameMode.creative", vec![])],
    );
    let mut broadcast = Text::translate(
        "chat.type.admin",
        vec![Text::literal("Server"), feedback.clone()],
    );
    broadcast.style = TextStyle {
        font: None,
        color: Some(TextColor::Gray),
        italic: Some(true),
        ..TextStyle::default()
    };

    let lang = table();
    let resolved = lodestone_game::text::resolve(&broadcast, &lang.translator());
    assert_eq!(
        resolved.to_plain_string(),
        "[Server: Set own game mode to Creative Mode]"
    );

    let spans = resolved.to_spans();
    assert!(
        !spans.is_empty(),
        "no spans at all means the assertions below are vacuous"
    );
    for span in &spans {
        assert_eq!(
            span.style.italic,
            Some(true),
            "every span of the op broadcast must be italic, including the nested \
             feedback message — span {:?} was not",
            span.text
        );
        assert_eq!(
            span.style.color,
            Some(TextColor::Gray),
            "and grey; span {:?} was not",
            span.text
        );
    }
    // The control: the same feedback message *outside* the broadcast wrapper is
    // not italic. Without this, a resolver that italicised everything
    // unconditionally would pass the loop above.
    let plain = lodestone_game::text::resolve(&feedback, &lang.translator());
    for span in plain.to_spans() {
        assert_ne!(
            span.style.italic,
            Some(true),
            "the feedback a command sends its own caller is not italic in vanilla \
             — only the op broadcast is"
        );
    }
}

/// A key the table does not have falls back to the key itself, not to empty —
/// vanilla's behaviour, and the right one because it makes a missing key
/// *visible*.
///
/// The empty-string arm is the control: a resolver that silently blanked a miss
/// would satisfy "does not contain real words" just as well.
#[test]
fn a_missing_key_shows_the_key_and_a_fallback_wins_over_it() {
    let missing = Text::translate("commands.lodestone.no.such.key", vec![]);
    assert_eq!(resolve(&missing), "commands.lodestone.no.such.key");
    assert_ne!(resolve(&missing), "");

    let mut with_fallback = Text::translate("commands.lodestone.no.such.key", vec![]);
    if let lodestone_model::TextContent::Translate { fallback, .. } = &mut with_fallback.content {
        *fallback = Some("a fallback".to_owned());
    } else {
        panic!("Text::translate must build a Translate node");
    }
    assert_eq!(resolve(&with_fallback), "a fallback");
}

/// The drift gate: every pattern [`PATTERNS`] transcribes must equal the real
/// `assets/minecraft/lang/en_us.json`.
///
/// `#[ignore]`d because it reads `client.jar`, and **fail-closed** — a missing
/// jar is a failure, never a skip, for the reason `container_slot_sprites.rs`
/// spells out. This is what keeps the always-on tests above honest: they expand
/// transcribed patterns by hand, and this is the only thing that says the
/// transcription still matches the game.
#[test]
#[ignore = "requires the vanilla client.jar"]
fn transcribed_patterns_match_the_real_en_us_json() {
    let root = pack_root().expect(
        "no vanilla pack found; set LODESTONE_ASSETS to a root holding client.jar \
         — do NOT treat a missing jar as a pass",
    );
    let bytes = std::fs::read(root.join("client.jar")).expect("client.jar is readable");
    let zip = lodestone_assets::ZipSource::from_bytes(bytes).expect("client.jar is a zip");
    let lang = Language::en_us_from_source(&zip)
        .expect("en_us.json parses")
        .expect("client.jar carries assets/minecraft/lang/en_us.json");

    // The whole-table sanity floor, so a truncated or wrong-namespace read cannot
    // pass the per-key loop by having every key absent.
    assert!(
        lang.len() > 8000,
        "en_us.json looks truncated: {} keys (26.2 ships 8123)",
        lang.len()
    );

    let mut wrong: Vec<String> = Vec::new();
    for (key, transcribed) in PATTERNS {
        match lang.get(key) {
            Some(real) if real == *transcribed => {}
            Some(real) => wrong.push(format!("{key}: transcribed {transcribed:?}, jar {real:?}")),
            None => wrong.push(format!("{key}: absent from the jar's en_us.json")),
        }
    }
    assert!(wrong.is_empty(), "PATTERNS has drifted:\n  {}", wrong.join("\n  "));
}

/// The same version-free pack discovery `resources::asset_root` performs — env
/// override, else the highest-sorting `.cache/mc/<ver>` holding a `client.jar`,
/// searched upward from the current directory so this works whether it runs from
/// the crate or the workspace root.
///
/// Duplicated rather than shared because `resources::vanilla_manager` is
/// `pub(crate)` and this is an integration test. Narrower than the production
/// rule on purpose: only `client.jar` is needed here, not
/// `generated/reports/blocks.json`.
fn pack_root() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("LODESTONE_ASSETS") {
        let path = std::path::PathBuf::from(dir);
        return path.join("client.jar").is_file().then_some(path);
    }
    let cwd = std::env::current_dir().ok()?;
    for base in cwd.ancestors() {
        let cache = base.join(".cache/mc");
        let mut roots: Vec<std::path::PathBuf> = std::fs::read_dir(&cache)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.join("client.jar").is_file())
            .collect();
        roots.sort();
        if let Some(root) = roots.pop() {
            return Some(root);
        }
    }
    None
}
