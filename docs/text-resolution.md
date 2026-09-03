# Text resolution

## What it is

A chat component arrives from the wire as *structure* — translation keys and
placeholders — and something has to turn it into words before it is drawn.
`ResolvedText` makes that step a type rather than a convention: the styled
flatteners live on it, so a surface that forgot to consult the language table
fails to compile instead of drawing a raw key.

## How it works

`lodestone_model::Text` is the component tree every wire format parses into
(`Text::from_json`, `Text::from_nbt`, `Text::from_legacy`). It carries literal
content or a `translate` key with arguments, a partial style, `extra` children
and optional `click`/`hover`/`insertion`.

`Text::resolve(&self, translate)` walks that tree and replaces every `translate`
node with the equivalent literal subtree — the pattern's text becomes the node's
own content plus a run of children, and each substituted argument keeps its own
style while inheriting the resolving node's. It returns `ResolvedText`, a
newtype whose inner tree contains no `translate` node anywhere.

The split of methods is the whole mechanism:

| on `Text` | on `ResolvedText` |
|---|---|
| `from_json`, `from_nbt`, `from_legacy` | `literal`, `from_legacy` |
| `to_plain_string`, `to_plain_string_with` | `to_plain_string` |
| `resolve` | `to_spans`, `to_interactive_spans`, `to_legacy_string` |

`Text` keeps only the plain flatteners, which are for logs, panics, tests and
the cross-format oracle — places where a key is a perfectly good identifier.
Everything that produces *styled runs*, and therefore everything that reaches a
pixel, is on `ResolvedText`. There is no constructor from an arbitrary `Text`:
the only ways in are a real resolution pass, a literal string, or a legacy
`§`-coded string (a format with no representation for a key).

`ResolvedText` derefs to `Text`, so the safe direction — handing a resolved tree
to something that only wants a component — costs nothing.

### The gate

`ResolvedText`'s own doc carries a `compile_fail` doctest calling `to_spans` on
an unresolved tree, paired with the resolved form as an ordinary doctest.
The pair is the control: swap the `compile_fail` body for the resolved call and
the doctest fails with "Test compiled successfully, but it's marked
`compile_fail`", which is what makes the rejection evidence rather than an
assertion about a name that happens not to exist.

## Where the table comes from

`lodestone_assets::Language::translator` produces the closure. In the shell it
is `Sim::translator`, which yields an empty closure when no pack is loaded.

Resolution happens as late as the table is still in hand, and no later:

- **Chat** resolves at *read* (`ChatLog::recent_spans` and siblings take the
  table), not at ingest, so a language pack pushed mid-session re-reads lines
  already in the scrollback.
- **Nametags** resolve inside `fold_entities_for_local`, which takes the table
  because the fold is the last point that holds one.
- **Command-suggestion tooltips** resolve at `SuggestionRequests::receive`, the
  single point a server-authored tooltip enters the shell.
- **Item hover names** resolve inside `lodestone_game::item`'s shared builder,
  which already had the table because a base display name *is* a key.
- **Session end reasons** are `ResolvedText` in `SessionEnd` itself.

### Resolving against the empty table

`text.resolve(&|_| None)` is a real resolution: every key lowers to its own
name, so the result is genuinely literal and the flatteners accept it. It is the
honest form where no table exists, and each such site says why:

- a server-list MOTD, decoded before any pack is loaded;
- display-entity text, extracted by a scheduled system where the session table
  is not a world resource;
- item tooltips, whose module has no table at all;
- protocol-family test asserts, whose subjects are literals.

The first three are the places to revisit if those surfaces should read
translated text; threading a table to them is the change, not a new constructor.

## How to change it

- **Adding a draw surface**: take `&ResolvedText` (or `&[TextSpan]` derived from
  one). If the type is inconvenient, the table is missing further up — thread it
  rather than resolving against `&|_| None` at the draw call, which is where the
  guarantee stops meaning anything.
- **Adding a decoder**: keep producing `Text`. The wire boundary is exactly
  where the unresolved form belongs.
- **`to_spans_ignoring_legacy_codes`** stays crate-private to `lodestone-model`
  and is still guarded by `tests/legacy_expansion_guard.rs`; a render surface
  that reaches it draws `§7` as two glyphs.

## Configuration

None. The language table is data, passed as a closure.

## Dependencies

`lodestone-model` owns `Text`, `ResolvedText` and the resolution walker, and
depends on nothing for it. `lodestone_assets::Language` supplies real tables;
`lodestone_game::text::interactive_spans` adds the `click`/`hover`-carrying
flatten a chat hit-test needs on top of `ResolvedText::to_interactive_spans`.
