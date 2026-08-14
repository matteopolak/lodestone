# The version-free part of each adapter's helper prologue

## What it is

`crates/lodestone-core/src/lib.rs` now exports `encode_body`, `decode_body`,
`decode_body_exact`, and `unpack_degrees` — the substance of four small helpers that used to
be hand-copied, byte-for-byte, at the top of each version crate's adapter
(`crates/protocol/{v47,v340,v735}/src/adapter.rs`; `v770`'s equivalent now lives in
`crates/protocol/v770/src/adapter/mod.rs`, since that crate's adapter was later split into a
directory module). Each version crate keeps a same-named local wrapper that is now one line.

A protocol-architecture survey identified these as safe to share because they carry no
version-specific behavior and touch no enforced invariant (folder-level deletability, or the
version-crate/shared-crate dependency direction `xtask check-isolation` enforces). The survey
also named `send`, `json_reason_text`, `game_mode`, and `dimension_id` as candidates — **those
did not move**, and the reason why is the more useful part of this doc.

## How it works

### What actually moved, and why the signature changed

```rust
// crates/lodestone-core/src/lib.rs
pub fn encode_body<T: Encode>(packet: &T, ctx: Ctx) -> std::result::Result<Vec<u8>, String>;
pub fn decode_body<T: Decode>(payload: &[u8], ctx: Ctx) -> std::result::Result<T, String>;
pub fn decode_body_exact<T: Decode>(payload: &[u8], ctx: Ctx) -> std::result::Result<T, String>;
pub fn unpack_degrees(packed: i8) -> f32;
```

These return `String` on error, not `lodestone_model::AdapterError`. `AdapterError` lives in
`lodestone-model`, which already depends on `lodestone-core` — so the reverse edge is a Cargo
dependency cycle, not merely a style violation. `String` is what lets the codec logic itself
(the `Writer`/`Reader` construction, the `ensure_empty` trailing-byte check) live in
`lodestone-core` while each crate's `AdapterError`-typed wrapper stays local:

```rust
// crates/protocol/v47/src/adapter.rs — identical shape in v340, v735, and (for
// encode_body/decode_body only) v770
fn encode_body<T: Encode>(packet: &T) -> Result<Vec<u8>, AdapterError> {
    lodestone_core::encode_body(packet, CTX).map_err(AdapterError::Encode)
}
```

`v770`'s own `decode_body_exact`-equivalent (`decode_full`, using a `dec_err` helper) was left
alone — it was never byte-identical to the other three's `decode_body_exact` (different name,
different doc, different error plumbing), so moving it was never in scope. Its `unpack_degrees`
lives in `packets::entity`, a different module entirely.

### Why `send`, `json_reason_text`, `game_mode`, and `dimension_id` did not move

Checked directly rather than assumed, because byte-identity was the entire justification for
moving anything, and the survey's own line citations for `encode_body`/`decode_body`/`send`
turned out to only actually establish identity for `encode_body`, `decode_body`, and (in three
of four crates) `decode_body_exact`:

| helper | blocked by | where that type lives |
|---|---|---|
| `send` | its return type is `Directive` | `lodestone-model` |
| `json_reason_text` | its return type is `Text` | `lodestone-model` |
| `game_mode` | its return type is `GameMode` | `lodestone-model` |
| `dimension_id` | signature differs per family (already excluded by the survey itself) | — |

The three blocked helpers' **success value**, not just their error variant, is a
`lodestone-model` type. That is what makes them structurally different from `encode_body` and
friends: a `String`-typed error can rescue a function whose payload is a primitive
(`Vec<u8>`, `T: Decode`, `f32`), but there is no equivalent trick when the payload itself is the
type that cannot be named from `lodestone-core`. Moving `Directive`/`Text`/`GameMode` down into
`lodestone-core` would fix this, but that is a change to `lodestone-model`, which is out of
scope for this task (and, independently, `lodestone-model` and `send`/`json_reason_text`
/`game_mode`'s call sites are the ones that would need re-review, not a mechanical move).

`send` is four lines in each of the four crates and stays that way — there is no `String`-typed
version of "build a `Directive::Send`" that would still be useful to a caller.

## How to change it

Adding a fifth codec helper that looks identical across families: check its return type before
assuming it can move. If it returns, or contains, a type from `lodestone-model`
(`Directive`, `AdapterError`, `Text`, `GameMode`, `DimensionId`, or anything under that crate's
own modules), it cannot move to `lodestone-core` as-is. Either the value itself is a primitive
and only the error needs downgrading to `String` (the pattern used here), or the helper cannot
move without a change to `lodestone-model` that is a separate decision.

Both `xtask check-isolation` and `xtask check-deletable <family>` must stay green after any
change here — `lodestone-core` is depended on by every version crate, so a mistake is visible
everywhere at once, and neither check can be skipped in favor of just running the affected
crate's own tests.

## Configuration

None — no feature flags or env vars gate any of this.

## Dependencies

`lodestone-core` gained no new dependencies. The moved helpers only use `Ctx`, `Writer`,
`Reader`, `Encode`, and `Decode`, all already defined there before this change.
