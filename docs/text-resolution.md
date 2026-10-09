# Text resolution

## What it is

A chat component arrives from the wire as structure (translation keys and placeholders) and must become words before it is drawn. `ResolvedText` makes that a type: the styled flatteners live on it, so a surface that skipped the language table fails to compile instead of drawing a raw key.

## How it works

- `lodestone_model::Text` is the tree every wire format parses into (`from_json`, `from_nbt`, `from_legacy`): literal content or a `translate` key with arguments, a partial style, `extra` children and optional `click`/`hover`/`insertion`.
- `Text::resolve(&self, translate)` replaces every `translate` node with an equivalent literal subtree (the pattern text becomes the node's content plus child runs; each argument keeps its style while inheriting the node's) and returns `ResolvedText`, a newtype whose tree has no `translate` node.

| on `Text` | on `ResolvedText` |
|---|---|
| `from_json`, `from_nbt`, `from_legacy` | `literal`, `from_legacy` |
| `to_plain_string`, `to_plain_string_with` | `to_plain_string` |
| `resolve` | `to_spans`, `to_interactive_spans`, `to_legacy_string` |

`Text` keeps only plain flatteners (logs, panics, tests, the cross-format oracle, where a key is a fine identifier); everything producing styled runs, and so every pixel, is on `ResolvedText`. The only ways in are a real resolution pass, a literal string, or a legacy `§`-coded string. `ResolvedText` derefs to `Text`, so passing it onward is free.

- **Gate.** `ResolvedText`'s doc has a `compile_fail` doctest calling `to_spans` on an unresolved tree, paired with the resolved form as an ordinary doctest. Swapping the `compile_fail` body for the resolved call makes it fail with "compiled successfully, but marked `compile_fail`", which is what makes the rejection evidence.
- **Where the table comes from.** `lodestone_assets::Language::translator` yields the closure; in the shell it is `Sim::translator` (empty with no pack). Resolve as late as the table is in hand: chat at read (`ChatLog::recent_spans` and siblings, so a mid-session language pack re-reads scrollback), nametags in `fold_entities_for_local`, command-suggestion tooltips at `SuggestionRequests::receive`, item hover names in `lodestone_game::item`'s shared builder, and session end reasons are `ResolvedText` in `SessionEnd`.
- **Empty table.** `text.resolve(&|_| None)` is a real resolution (each key lowers to its own name), the honest form where no table exists: server-list MOTDs (decoded before any pack), display-entity text (scheduled system without the session table), item tooltips (no table in that module) and protocol-family test asserts. The first three are the places to thread a table if they should show translated text.

## How to change it

- New draw surface: take `&ResolvedText` or `&[TextSpan]` derived from one. If that is inconvenient, thread the table from further up rather than resolving against `&|_| None` at the draw call, where the guarantee stops meaning anything.
- New decoder: keep producing `Text`; the wire boundary is where the unresolved form belongs.
- `to_spans_ignoring_legacy_codes` stays crate-private to `lodestone-model` and guarded by `tests/legacy_expansion_guard.rs`; a render surface reaching it draws `§7` as two glyphs.

## Configuration

None. The language table is data passed as a closure.

## Dependencies

`lodestone-model` owns `Text`, `ResolvedText` and the walker. `lodestone_assets::Language` supplies tables; `lodestone_game::text::interactive_spans` adds the click and hover flatten a chat hit-test needs.
