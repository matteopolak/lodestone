# The "Play offline" identity

## What it is

The one persisted, user-editable name the client joins under when no Microsoft
account is signed in, plus the stable UUID derived from it. It lives in
`crates/lodestone-shell/src/offline_identity.rs` and is what
`net.rs`'s login-start packet carries for **every** join this shell makes today,
singleplayer and multiplayer alike.

Before it existed, the offline arm of `NetClient`'s profile match read

```rust
username: unique_username(),      // from `lodestone-testsupport`
uuid: uuid::Uuid::new_v4(),
```

which gave the player a **new offline account on every launch**, twice over.
`unique_username` is a *test* helper: it cannot return the same name twice, by
construction (a process-wide atomic counter, the pid, and a coarse clock) and by
its own test. `new_v4` is random. The owner's report — *"I keep spawning in the
air even if I rejoin"* — was the visible half; the invisible half is that no
per-player save could ever be found once the server writes one, because the key
changes before the next launch reads it.

## How it works

### The stored value is the name, and only the name

`offline.json`, beside `profiles.json` / `servers.json` / `options.json` in
`lodestone_auth::paths::data_dir()`:

```json
{
  "username": "Player"
}
```

Missing, unreadable, malformed, or holding a name a server would reject is the
default (`Player`) — the same tolerance rule `AccountsMetadata`, `Options` and
`Keybinds` follow. Refusing to start over a corrupt preferences file is worse
than playing under the placeholder name, and a name that *reaches* the
login-start packet and gets rejected is a disconnect with no obvious cause, so
validity is re-checked on load as well as on set.

### The UUID is derived, never stored

`offline_uuid(name)` reproduces Java's
`UUID.nameUUIDFromBytes(("OfflinePlayer:" + name).getBytes(UTF_8))`: MD5 of
those bytes, with the version nibble forced to 3 and the two variant bits to
RFC 4122. That is exactly how an offline-mode server computes the account id it
persists player data under, so the id we present is the id vanilla would have
assigned us.

Storing the UUID alongside the name would let the two drift; a derivation
cannot.

### Why the UUID mattered, not just the name

`CLAUDE.md` records that *offline mode derives the account UUID from the
username, ignoring the UUID the client sends*. That is true of **vanilla** and
is why the name has to be stable. It is **not** true of our own integrated
server: `lodestone-server`'s login handler does `login_uuid = Some(uuid)` — it
echoes back whatever the client presented and keys the player entity on it
(issue #438). So for singleplayer a stable *name* alone would have fixed
nothing; the random `new_v4` was the operative instability there.

In the other direction, against real vanilla, our client **discards** the
profile in `LOGIN_FINISHED` (`v770`'s `handle_login` binds it to `_profile`), so
`NetClient::local_uuid` keeps whatever we sent. Presenting a random v4 meant the
client's idea of its own identity disagreed with the server's for the entire
session — latent in anything keyed on "am I this player?", issue #189's roster
exclusion included. Deriving the UUID vanilla's way makes the two agree by
construction. **Fixing the discard itself is a `crates/protocol/**` change and
has not been made.**

### The API

| item | for |
|---|---|
| `OfflineIdentity::load()` / `save()` | production; resolve against the real data directory |
| `OfflineIdentity::load_from(&Path)` / `save_to(&Path)` | tests; the twins that cannot touch the developer's file |
| `OfflineIdentity::set_username(&str)` | the validating door — refuses and leaves the old name live |
| `OfflineIdentity::from_username_unchecked(String)` | a caller that already has a name and is not storing it (`connect_as`) |
| `offline_uuid(&str)` | the derivation, standalone |
| `validate_username(&str)` | what a UI should call before storing a typed name |

`NetClient::connect` / `open_singleplayer` load the persisted identity.
`NetClient::connect_as` (and `Sim::connect_as`) take an explicit name — see
below.

## Why not a row in `profiles.json`

The obvious model is "the offline placeholder becomes a persisted profile entry
like any other, so the account switcher's existing listing and selection
machinery carries it". It was considered and rejected, for three reasons that
are properties of `AccountsMetadata`, not preferences:

1. **`AccountProfile` is keyed by `profile_id`, and an offline UUID is a
   function of the name.** Renaming would change the key, so `upsert` would
   produce a *second* entry rather than editing the first.
2. **`selected: None` already means offline mode** — `menu/accounts.rs`'s
   offline row shows its marker exactly when `selected.is_none()`. Adding a row
   would create two ways to express one state.
3. **The offline placeholder is not an account.** No Mojang profile id, no skin
   URL, no keychain entry, no meaningful `last_used`. Three of
   `AccountProfile`'s four fields would be dead.

**There is no way to distinguish an offline entry from a Microsoft one in
`profiles.json` today** — that is the real schema gap `menu/accounts.rs`'s
module docs already record — and this change does not close it. It routes around
it instead: a single-valued setting in its own file, which needs no schema
change in `lodestone-auth` and no coordination with the switcher.

## Live gates, and the helper that must not come back

`unique_username` exists for a real reason. Offline mode shares one persisted
player file per name, and **a dead player is held on the death screen, which
sends no chunks** — a silent, total chunk blackout while join, keep-alives and
entity movement all continue perfectly. Every live gate needs a fresh identity
per run, and two gates running concurrently under one name evict each other.

So the freshness moved to where it belongs: `NetClient::connect_as` /
`Sim::connect_as` take the name explicitly, and every live gate in
`crates/lodestone-shell/tests/` passes `unique_username()` there (16 call sites
across 14 files). Production goes through `connect` / `open_singleplayer`, which
are stable by design.

Two layers stop the helper reaching production again:

1. **Cargo.** `lodestone-testsupport` is now a `[dev-dependencies]` entry of
   `lodestone-shell`. It was the only crate in the workspace where it sat under
   plain `[dependencies]`: 11 others have it under `[dev-dependencies]`, and
   `lodestone-sound` has it `optional` behind its `live-v770` feature and names
   it only from `tests/`. The lib target cannot
   name it, so the defect is a compile error rather than a convention.
   `#[cfg(test)] mod tests` inside `src/` still sees it — unit tests are part of
   the lib *test* target — so `net.rs`'s, `app/tests.rs`'s and `sim/tests.rs`'s
   own gates are unaffected.
2. **A source scan**, `tests/no_production_source_names_testsupport.rs`, for the
   case where someone adds the normal edge back — a one-line Cargo change that
   makes the compile error disappear and looks innocuous in a diff. It flags any
   mention of the underscored crate path under `src/` outside a `#[cfg(test)]`
   region, with a control for the needle and one for each of the two exclusions.

## How to change it

- **Do not "simplify" `offline_uuid` to `Uuid::new_v3`.** That is a *namespaced*
  v3 (`md5(namespace_bytes ‖ name)`); Java's `nameUUIDFromBytes` hashes the name
  bytes alone. The wrong one still produces a stable, plausible, version-3 UUID,
  so no stability test can tell them apart — only the exact vectors can, and
  `offline_identity::tests::the_namespaced_v3_reading_disagrees_with_every_vector`
  is the control that says so.
- **Do not replace the hand-written UUID vectors with calls to `offline_uuid`.**
  They come from CPython's `hashlib.md5` — a second implementation of the same
  published rule, which is what keeps the expectation outside the code under
  test. There is no JVM on this machine, so a real `nameUUIDFromBytes` oracle
  run is unavailable.
- **A new stored field** goes into `from_json` and `to_json` together, keeping
  per-field tolerance: one bad field must not cost the others.
- **The no-argument `load`/`save` touch the real data directory.** Tests use the
  `_from`/`_to` twins, the same split `saves.rs` uses.
- **The name is sent to every server the player joins.** Do not default it from
  `$USER` or any other machine-derived string.

### The editor

`crates/lodestone-shell/src/menu/accounts.rs` owns it; the frames are in
`menu/render/account_screen.rs`. Three moving parts:

1. **`AccountsNav` holds the identity.** `with_path` loads it from
   `offline.json` **in the directory `profiles.json` came from**, never from
   `offline_identity_path()`. That is the same structural defence `MenuNav` uses
   for its saves root: a test that hands in a temp `profiles.json` cannot reach
   the developer's real `offline.json` even by accident, and nothing in the test
   suite names the no-argument `load`.
2. **The offline row's label *is* the name.** It used to be the literal
   `"Play offline"`, which is why this module's whole model — stored, validated,
   UUID-derived — reached **zero pixels**. `offline_username()` returns a
   `String` rather than the `&str` the original patch note asked for: the state
   is behind a `RefCell`, so a borrow cannot outlive the guard.
3. **The affordance is the third footer button, which changes identity.** It
   reads `Remove` on an account row and `Edit Name` on the offline row.
   `AccountsNav::third_button` is the single expression the caption and
   `activate_button`'s dispatch share.

**Why not a fifth footer button** — a measurement, not a preference.
`ACCOUNTS_BUTTON_W` is 74 px with `spacing(4)`, so four buttons measure
`4 * 74 + 3 * 4 = 308`, inside `config::MIN_SCALED_WIDTH`'s 320. Five would
measure `5 * 74 + 4 * 4 = 386` and hang 33 px off *each* edge at the smallest
supported GUI scale. The third slot was already dead for the offline row — it
cannot be removed, so the button was drawn inactive whenever the cursor sat on
it — which is exactly the row that needs an Edit affordance. The key hint's
middle term follows the same predicate, because `Del remove` was a *lie* on that
row before this.

The editor itself is a real `EditBox` (so the caret, the selection and the
horizontal scroll are `edit_box.rs`'s arithmetic, not restated), capped at 16
`char`s, seeded from the stored name with the caret at the end. Enter commits,
Escape abandons, and everything else goes to the box — the editor's branch is
**first** in `handle_key_with`, so `Delete` deletes a character rather than
removing an account. A refusal shows `NameError`'s own `Display` and leaves the
editor open with the old name still live, which `set_username` guarantees. The
frame also shows `offline_uuid` of the **typed** name, live, because the name
*is* the identity and that consequence should be visible before Done.

**Do not hand-roll validation in the menu.** `validate_username` mirrors the
server's rule; a second copy would drift. The box's own length cap is not a
second copy — it stops the 17th keystroke rather than deciding whether a name is
legal, and the commit still calls `set_username`.

### Residual gaps

- **The stored-name world is not exercised through production's `load()`.**
  `std::env::set_var` is `unsafe` under this workspace's `deny(unsafe_code)`, so
  a test cannot point `LODESTONE_DATA_DIR` at a fixture. What is untested is the
  path join only — the parse is covered hermetically against `load_from`, which
  is the function `load` delegates to.
- **Player-state persistence is a separate piece of work.** `playerdata` /
  `save_player` have zero hits in `crates/lodestone-server/src/`: position,
  health and inventory are not persisted at all, which is the other half of why
  the owner respawns at world spawn. A stable identity is the prerequisite that
  makes a persisted player file findable, not the fix for its absence.
- **Online-mode join is still an island.** `NetClient::connect_online` has zero
  callers in the shell and `lodestone_auth::login::try_cached_session` is called
  from nowhere in `crates/lodestone-shell/src/`, so the `Some(session)` arm of
  the profile match is unreachable from production. A signed-in Microsoft
  account in the switcher is not used by any join. That is issue #66's remaining
  half.

## Configuration

`LODESTONE_DATA_DIR` relocates the whole data directory, and therefore
`offline.json` with it (`lodestone_auth::paths::data_dir`).

## Dependencies

`lodestone-auth` for the directory, `serde_json` for the file,
`lodestone-worldgen-core`'s `hash::md5` for the RFC 1321 digest (the
workspace's one MD5, already verified against the RFC's published vectors — a
transitive dependency already, so the edge adds no compilation), and
`lodestone-client`'s `LoginProfile` for the value `net.rs` wants.

## Gates

| gate | what it establishes |
|---|---|
| `offline_identity::tests::offline_uuid_matches_the_externally_computed_vectors` | the derivation is vanilla's, against five externally computed values |
| `offline_identity::tests::the_namespaced_v3_reading_disagrees_with_every_vector` | the plausible wrong derivation is distinguishable — control for the above |
| `tests/offline_identity_is_stable.rs` | two independent constructions agree, in **both** worlds (stored name, no file), with the pre-fix expression as the control |
| `net::tests::two_offline_sessions_publish_the_same_identity` | production's `NetClient::connect` actually consumes it — the island check, via the published `local_uuid` on a dead port |
| `net::tests::connect_as_varies_the_published_identity_with_the_name` | control for the above, and that live gates really do get their name through |
| `tests/no_production_source_names_testsupport.rs` | the test helper is not reachable from production source |
| `menu::render::tests::the_offline_row_carries_the_persisted_name_through_the_real_frame` | the label is the persisted name, through the real `accounts_idle_frame`, with a no-file root as the control |
| `menu::render::tests::frame_for_reaches_the_name_editor_and_still_stamps_the_frame` | the **island check**: `frame_for` — the function `app.rs` calls every frame — really reaches the editor's frame, and it is still `gui_scale`-stamped |
| `menu::nav::tests::clicking_the_offline_name_field_does_not_save_but_clicking_done_does` | `MenuNav::click`'s `Screen::Accounts` arm exists, so clicking the field is not "hover + Enter" (which saved) |
| `menu::accounts::tests::a_refused_name_keeps_the_old_one_live_shows_why_and_writes_nothing` | a refusal reports `NameError`'s own text, leaves the old name live and writes nothing, with a corrected commit as the control |
| `menu::accounts::tests::a_key_that_means_something_to_the_list_is_swallowed_by_the_open_editor` | the editor's branch is first in `handle_key_with`, so `Delete` cannot remove an account; the control removes one with the editor closed |
| `menu::accounts::tests::the_third_button_is_remove_for_an_account_and_edit_name_for_the_offline_row` | both arms of the shared predicate observed in one test |
