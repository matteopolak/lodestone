//! Tree-level behaviour, checked against Brigadier 1.3.10's own algorithms
//! (`CommandDispatcher::parseNodes`/`getCompletionSuggestions`,
//! `Suggestions::merge`), never against this crate's own parser or suggester
//! in isolation. Every expected value below is derived by hand from the
//! decompiled/upstream source, not from running this code and eyeballing the
//! result — see each test's comment for the derivation.

use std::sync::Arc;

use lodestone_command::{
    ArgumentType, ArgumentTypeRegistry, BoolArgument, CommandTree, IntegerArgument, ParseError, ParseErrorKind, ParsedValue, StringArgument, StringReader,
};

// ---------------------------------------------------------------------------
// Integer bounds: position semantics, not just pass/fail.
// ---------------------------------------------------------------------------

#[test]
fn integer_out_of_range_reports_start_not_end_of_token() {
    // Oracle: IntegerArgumentType.parse resets `reader`'s cursor to `start`
    // via `reader.setCursor(start)` *before* throwing integerTooHigh — so the
    // reported position is where the number began (0), not where it ended
    // (2), even though the number itself parsed successfully as a value.
    // The naive (wrong) hypothesis a first-pass port would reach for is
    // "position = end of the consumed number" (2); the oracle says 0.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "amount", Arc::new(IntegerArgument::bounded(0, 10)));
    tree.set_executable(arg, true);

    let err = tree.parse("15").unwrap_err();
    assert_eq!(err.position, 0, "expected oracle position 0 (start of token), rejecting the end-of-token hypothesis (2)");
    assert!(matches!(err.kind, ParseErrorKind::IntegerTooHigh { found: 15, max: 10 }));
}

#[test]
fn integer_immediately_followed_by_more_text_is_expected_separator_not_leftover() {
    // "12abc": readInt consumes only "12" (cursor -> 2; 'a' is not
    // `isAllowedNumber`), succeeding as a value — but the very next character
    // is neither a space nor end-of-input, so `parseNodes`'s boundary check
    // (`if (reader.canRead() && reader.peek() != ' ') throw
    // dispatcherExpectedArgumentSeparator()`) fires *before* anything gets a
    // chance to call this "leftover". Position is 2 (right after the
    // successfully-parsed number, where the missing space should have been).
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "amount", Arc::new(IntegerArgument::new()));
    tree.set_executable(arg, true);

    let err = tree.parse("12abc").unwrap_err();
    assert_eq!(err.position, 2);
    assert!(matches!(err.kind, ParseErrorKind::ExpectedArgumentSeparator));
}

#[test]
fn integer_leftover_input_after_a_real_separator_is_unknown_argument() {
    // "12 abc": this time the number IS followed by a real space, which gets
    // consumed (cursor -> 3); the `amount` node is a leaf (no children, no
    // redirect), so nothing exists to take "abc". `CommandDispatcher.execute`
    // sees leftover input with a non-empty context -> DISPATCHER_UNKNOWN_ARGUMENT
    // at the reader's cursor, which is 3 (the start of "abc"), not 0.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "amount", Arc::new(IntegerArgument::new()));
    tree.set_executable(arg, true);

    let err = tree.parse("12 abc").unwrap_err();
    assert_eq!(err.position, 3);
    assert!(matches!(err.kind, ParseErrorKind::UnknownArgument));
}

#[test]
fn integer_invalid_number_reports_start_via_reset_cursor() {
    // "999999999999abc" overflows i32 during readInt's own number-run (the
    // whole all-digit prefix is consumed as "allowed number" text before
    // Integer::parseInt fails), so this is InvalidInt, not a leftover error —
    // and per StringReader.readInt, the cursor resets to `start` (0) before
    // the exception is created.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "amount", Arc::new(IntegerArgument::new()));
    tree.set_executable(arg, true);

    let err = tree.parse("999999999999").unwrap_err();
    assert_eq!(err.position, 0);
    assert!(matches!(err.kind, ParseErrorKind::InvalidInt(ref s) if s == "999999999999"));
}

#[test]
fn unknown_command_at_root_reports_position_zero() {
    let tree = CommandTree::new();
    let err = tree.parse("nope").unwrap_err();
    assert_eq!(err.position, 0);
    assert!(matches!(err.kind, ParseErrorKind::UnknownCommand));
}

// ---------------------------------------------------------------------------
// The greedy-vs-single-word trap: two hypotheses that disagree only once
// input crosses a space.
// ---------------------------------------------------------------------------

#[test]
fn greedy_vs_single_word_disagree_on_multi_token_input() {
    // Brigadier oracle for "hello world" against `message: word`:
    //   StringArgumentType.word() -> reader.readUnquotedString() consumes
    //   "hello" (space is not in isAllowedInUnquotedString), cursor -> 5.
    //   parseNodes then requires a separator (peek() == ' ' at 5: yes),
    //   skips it (cursor -> 6). The "message" node is a leaf with no
    //   children and no redirect, so nothing consumes "world"; the top-level
    //   parse ends with the reader canRead() at cursor 6, context non-empty
    //   -> DISPATCHER_UNKNOWN_ARGUMENT at position 6.
    //
    // Same oracle for "hello world" against `message: greedy string`:
    //   StringArgumentType.greedyString() takes reader.getRemaining() ("hello
    //   world") unconditionally and sets the cursor to the end (11) — no
    //   leftover, Ok.
    //
    // These two predictions genuinely disagree (Err vs Ok) on the identical
    // input, which is exactly the property a single-token test input cannot
    // produce — a gate built only on inputs like "hello" would pass for
    // *both* hypotheses and prove nothing.
    let mut word_tree = CommandTree::new();
    let word_root = word_tree.root();
    let word_arg = word_tree.add_argument(word_root, "message", Arc::new(StringArgument::word()));
    word_tree.set_executable(word_arg, true);

    let word_err = word_tree.parse("hello world").unwrap_err();
    assert_eq!(word_err.position, 6, "rejected hypothesis: word-type consumes the whole line like greedy would");
    assert!(matches!(word_err.kind, ParseErrorKind::UnknownArgument));

    let mut greedy_tree = CommandTree::new();
    let greedy_root = greedy_tree.root();
    let greedy_arg = greedy_tree.add_argument(greedy_root, "message", Arc::new(StringArgument::greedy()));
    greedy_tree.set_executable(greedy_arg, true);

    let parsed = greedy_tree.parse("hello world").expect("greedy string must consume the whole line");
    assert_eq!(parsed.argument("message"), Some(&ParsedValue::String("hello world".to_string())));
}

#[test]
fn quotable_phrase_behaves_like_word_on_unquoted_multi_token_input() {
    // A second angle on the same trap: `quotable` and `word` are also
    // indistinguishable from each other on unquoted input — StringReader's
    // readString() only takes the quoted branch if the *next* char is a
    // quote, otherwise it falls through to readUnquotedString(), byte-for-
    // byte identical to the `word` case above. This is the negative half of
    // the trap: proving quotable is NOT secretly greedy either.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "message", Arc::new(StringArgument::quotable()));
    tree.set_executable(arg, true);

    let err = tree.parse("hello world").unwrap_err();
    assert_eq!(err.position, 6);
    assert!(matches!(err.kind, ParseErrorKind::UnknownArgument));
}

#[test]
fn quotable_phrase_spans_spaces_when_quoted() {
    // `"hello world"` (with literal quotes in the input): readString sees a
    // starting `"`, skips it, and readStringUntil('"') accumulates up to the
    // closing quote, spaces included. Input length is 13
    // (`"`,h,e,l,l,o,' ',w,o,r,l,d,`"`), so a fully successful parse leaves
    // the cursor at 13.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "message", Arc::new(StringArgument::quotable()));
    tree.set_executable(arg, true);

    let parsed = tree.parse("\"hello world\"").expect("quoted phrase must span the space");
    assert_eq!(parsed.argument("message"), Some(&ParsedValue::String("hello world".to_string())));
}

// ---------------------------------------------------------------------------
// Suggestions: exact ordered candidate lists, per Suggestions::merge's
// case-insensitive sort.
// ---------------------------------------------------------------------------

#[test]
fn suggestions_are_sorted_case_insensitively_and_filtered_by_prefix() {
    // Oracle: LiteralCommandNode.listSuggestions matches
    // `literalLowerCase.startsWith(remainingLowerCase)`; Suggestions.merge
    // sorts the merged set with `a.compareToIgnoreCase(b)`.
    // "gamemode" vs "gamerule": equal through "gam", then 'e' < 'r', so
    // "gamemode" sorts first. "give" doesn't start with "gam" at all, and
    // the bare `target` word argument contributes no suggestions (its
    // default `ArgumentType::suggest` is empty, matching every built-in
    // primitive type except bool).
    let mut tree = CommandTree::new();
    let root = tree.root();
    tree.add_literal(root, "gamemode");
    tree.add_literal(root, "gamerule");
    tree.add_literal(root, "give");
    tree.add_argument(root, "target", Arc::new(StringArgument::word()));

    assert_eq!(tree.suggest("gam"), vec!["gamemode".to_string(), "gamerule".to_string()]);
}

#[test]
fn suggestions_at_empty_input_list_every_matching_child() {
    let mut tree = CommandTree::new();
    let root = tree.root();
    tree.add_literal(root, "gamemode");
    tree.add_literal(root, "gamerule");
    tree.add_literal(root, "give");

    assert_eq!(tree.suggest(""), vec!["gamemode".to_string(), "gamerule".to_string(), "give".to_string()]);
}

#[test]
fn bool_argument_suggests_only_the_matching_literal() {
    // Oracle: BoolArgumentType offers "true"/"false" unconditionally; the
    // generic prefix filter (Suggestions builder) narrows to the ones whose
    // lowercase form starts with the typed prefix's lowercase form. "t"
    // matches only "true".
    let mut tree = CommandTree::new();
    let root = tree.root();
    let flag = tree.add_literal(root, "flag");
    tree.add_argument(flag, "value", Arc::new(BoolArgument));

    assert_eq!(tree.suggest("flag t"), vec!["true".to_string()]);
    // Trailing space, no partial token yet: both candidates, sorted
    // case-insensitively ('f' < 't').
    assert_eq!(tree.suggest("flag "), vec!["false".to_string(), "true".to_string()]);
}

// ---------------------------------------------------------------------------
// Redirect cycle: the "obvious hang" turns out to be structurally impossible
// for an ordinary tree once the separator-consumption gate is right (see
// `CommandTree::parse`'s doc comment on `after_match` and the crate doc's
// "known simplifications" section) — every redirect hop must consume at
// least one character before recursing again, so depth is always bounded by
// the input length. What the (node, cursor) guard actually earns its keep
// against is a *custom* `ArgumentType` (the plugin extension point) that
// moves the cursor backward, defeating that bound from outside `parse`'s own
// control. This section demonstrates exactly that, plus a control proving
// the guard doesn't misfire on an ordinary, well-behaved repeated redirect.
// ---------------------------------------------------------------------------

/// A deliberately adversarial `ArgumentType`, simulating a buggy or
/// malicious plugin-supplied type: it always reports success, and always
/// forces the cursor to a fixed `home` position regardless of where it was
/// called from, so repeated calls keep landing on the same absolute
/// position no matter how far the surrounding redirect has otherwise
/// "progressed".
struct RewindingArgument {
    home: usize,
}

impl ArgumentType for RewindingArgument {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        reader.set_cursor(self.home);
        Ok(ParsedValue::String(String::new()))
    }
}

#[test]
fn redirect_cycle_from_a_rewinding_argument_type_terminates_with_an_error() {
    // root -> argument(x: Rewinding{home: 0}) --redirect--> root, input " a".
    // Hop 1 at cursor 0: parse() forces cursor to `home` (0, a no-op the
    // first time), returns Ok. The next char (index 0) is a space, so the
    // separator check passes, it's skipped (cursor -> 1), and the redirect
    // to root is followed with key (root, 1).
    // Hop 2 at cursor 1: parse() forces the cursor *back* to 0 — a real
    // ArgumentType could never do this from legitimate parsing, since it
    // starts wherever the reader already was, but nothing stops one from
    // calling `set_cursor` regardless. From 0, the same space is seen again,
    // skipped again (cursor -> 1 again), and the redirect targets root with
    // key (root, 1) — already visited. That repeat is what the guard exists
    // to catch; without it, this would recurse identically forever.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let x = tree.add_argument(root, "x", Arc::new(RewindingArgument { home: 0 }));
    tree.set_redirect(x, root);

    let start = std::time::Instant::now();
    let err = tree.parse(" a").unwrap_err();
    // Not a timing assertion about the algorithm's complexity — just cheap
    // insurance that the test process itself didn't hang if the guard were
    // ever removed by a future edit; the guard is what actually proves
    // termination, not the wall clock.
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(err.position, 1);
    assert!(matches!(err.kind, ParseErrorKind::RedirectCycle));
}

#[test]
fn control_well_behaved_repeated_redirect_to_root_is_not_treated_as_a_cycle() {
    // Same shape — an argument node that redirects back to the root — but
    // with a normal, consuming argument type (`word`) instead of one that
    // rewinds. This is the pattern every vanilla `/execute ... run <command>`
    // chain actually uses (repeatedly redirecting to the root), and it must
    // NOT trip the guard: each hop consumes a real token plus its separator,
    // so the (node, cursor) pair the guard tracks is different every time.
    // If the guard were instead something cruder like "root visited twice",
    // this control would wrongly fail on the second word; it doesn't, which
    // is the evidence the guard is keyed on (node, cursor) and not just node.
    let mut tree = CommandTree::new();
    let root = tree.root();
    let x = tree.add_argument(root, "x", Arc::new(StringArgument::word()));
    tree.set_executable(x, true);
    tree.set_redirect(x, root);

    let parsed = tree.parse("a b c").expect("repeated non-rewinding redirects to the root must not be treated as a cycle");
    // Each hop re-enters root fresh, so "arguments" collects one "x" per
    // token consumed along the path.
    assert_eq!(parsed.arguments.iter().filter(|(name, _)| name == "x").count(), 3);
}

// ---------------------------------------------------------------------------
// Custom argument type registration — a way for a plugin to
// register a custom ArgumentType with the same two functions.
// ---------------------------------------------------------------------------

struct UpperCaseWord;

impl ArgumentType for UpperCaseWord {
    fn parse(&self, reader: &mut StringReader) -> Result<ParsedValue, ParseError> {
        Ok(ParsedValue::Custom(reader.read_unquoted_string().to_uppercase()))
    }

    fn suggest(&self, _partial: &str) -> Vec<String> {
        vec!["ALPHA".to_string(), "BETA".to_string()]
    }
}

#[test]
fn custom_argument_type_registers_and_parses_and_suggests() {
    let mut registry = ArgumentTypeRegistry::new();
    registry.register("uppercase_word", Arc::new(UpperCaseWord));

    let custom = registry.get("uppercase_word").expect("just registered");
    let mut tree = CommandTree::new();
    let root = tree.root();
    let arg = tree.add_argument(root, "value", custom);
    tree.set_executable(arg, true);

    let parsed = tree.parse("hello").unwrap();
    assert_eq!(parsed.argument("value"), Some(&ParsedValue::Custom("HELLO".to_string())));

    assert_eq!(tree.suggest("b"), vec!["BETA".to_string()]);
    assert!(registry.get("nonexistent").is_none());
}
