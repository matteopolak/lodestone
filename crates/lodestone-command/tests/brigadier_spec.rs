//! Tree-level behaviour, checked against the command-parser library's own
//! node-parsing, completion-suggestion, and suggestion-merge algorithms,
//! never against this crate's own parser or suggester in isolation. Every
//! expected value below is derived by hand from the upstream algorithm, not
//! from running this code and eyeballing the result — see each test's
//! comment for the derivation.

use std::sync::Arc;

use lodestone_command::{
    ArgumentType, ArgumentTypeRegistry, BoolArgument, CommandTree, IntegerArgument, ParseError, ParseErrorKind, ParsedValue, StringArgument, StringReader,
};

// ---------------------------------------------------------------------------
// Integer bounds: position semantics, not just pass/fail.
// ---------------------------------------------------------------------------

#[test]
fn integer_out_of_range_reports_start_not_end_of_token() {
    // Oracle: the integer-argument parser resets the reader's cursor back to
    // `start` before raising the too-high error — so the
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
    // "12abc": the integer reader consumes only "12" (cursor -> 2; 'a' is not
    // an allowed number character), succeeding as a value — but the very next character
    // is neither a space nor end-of-input, so the node-parsing boundary check
    // (there is more input, and the next character is not a space: raise
    // an expected-argument-separator error) fires *before* anything gets a
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
    // redirect), so nothing exists to take "abc". Top-level execution
    // sees leftover input with a non-empty context -> an unknown-argument error
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
    // "999999999999abc" overflows i32 during the integer reader's own number-run (the
    // whole all-digit prefix is consumed as "allowed number" text before
    // the integer parse fails), so this is InvalidInt, not a leftover error —
    // and the reader resets its cursor to `start` (0) before
    // the error is created.
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
    // Oracle for "hello world" against `message: word`:
    //   the word-string reader consumes
    //   "hello" (space is not an allowed unquoted-string character), cursor -> 5.
    //   node-parsing then requires a separator (next char is a space at 5: yes),
    //   skips it (cursor -> 6). The "message" node is a leaf with no
    //   children and no redirect, so nothing consumes "world"; the top-level
    //   parse ends with input still remaining at cursor 6, context non-empty
    //   -> an unknown-argument error at position 6.
    //
    // Same oracle for "hello world" against `message: greedy string`:
    //   the greedy-string reader takes the whole remaining input ("hello
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
    // indistinguishable from each other on unquoted input — the quotable-string
    // reader only takes the quoted branch if the *next* char is a
    // quote, otherwise it falls through to the same unquoted-string reading, byte-for-
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
    // `"hello world"` (with literal quotes in the input): the quotable-string
    // reader sees a
    // starting `"`, skips it, and accumulates characters up to the
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
// Suggestions: exact ordered candidate lists, per the suggestion-merge
// case-insensitive sort.
// ---------------------------------------------------------------------------

#[test]
fn suggestions_are_sorted_case_insensitively_and_filtered_by_prefix() {
    // Oracle: a literal node's suggestions match by lowercased-prefix
    // containment; the merged suggestion set is
    // sorted case-insensitively.
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
    // Oracle: the bool-argument type offers "true"/"false" unconditionally; the
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

// ---------------------------------------------------------------------------
// Redirect-walk depth: a redirect hop consumes at least one character, which
// keeps a redirect cycle merely deep rather than infinite, but a command
// still arrives with up to the protocol's 32767-character cap — plenty of
// room for a redirect chain deep enough to overflow a recursive walk's call
// stack before the input itself runs out.
// ---------------------------------------------------------------------------

#[test]
fn control_self_redirecting_literal_survives_a_deep_redirect_chain() {
    // A literal that redirects straight back to the root, fed a chain long
    // enough to drive the redirect walk far deeper than an ordinary command
    // ever goes. Every hop consumes its literal token plus one separator, so
    // this cannot be mistaken for the (node, cursor) redirect-cycle case
    // covered above — each hop's key is distinct.
    //
    // Before the redirect walk was made iterative, this many hops overflowed
    // the call stack and aborted the process well short of completing (the
    // crate doc's `docs/server-commands.md` entry measured 1024 hops
    // surviving and fewer than 2048 overflowing, on a 2 MiB stack). This test
    // is the control: it must run to completion rather than crash, and it is
    // the regression test for the iterative rewrite that removed the bound
    // on stack depth rather than merely raising it.
    const HOPS: usize = 20_000;

    let mut tree = CommandTree::new();
    let root = tree.root();
    let a = tree.add_literal(root, "a");
    tree.set_executable(a, true);
    tree.set_redirect(a, root);

    let input = vec!["a"; HOPS].join(" ");
    let parsed = tree.parse(&input).expect("a long non-cyclic redirect chain must parse rather than abort");
    assert_eq!(parsed.nodes.len(), HOPS);
    assert!(parsed.nodes.iter().all(|&id| id == a));
}
