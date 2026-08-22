# Which player a join presents

## What it is

The single producer of the local player's username and UUID —
`crates/lodestone-shell/src/join_identity.rs`. Every production join, remote and
singleplayer alike, asks `join_identity::join_identity()` who it is, and gets one
answer: the account the switcher has selected, or the persisted "Play offline"
identity when there is none.

It exists because there used to be two answers and they disagreed. The account
switcher wrote `profiles.json`; `NetClient::open_singleplayer` read
`offline.json`; and the local player's *skin*, cached at sign-in as
`<data_dir>/skin.png`, followed the Microsoft account. So a singleplayer session
drew the signed-in player's skin above the offline player's name, and keyed the
server-side player file on the offline UUID. The owner reported exactly that:
selecting a Microsoft account changed the skin and nothing else.

## How it works

`join_identity::resolve` is the whole decision and it is pure — two files in,
one `JoinIdentity` out, no network and no keychain:

| `AccountsMetadata::selected` | `profiles.json` has the row | outcome |
|---|---|---|
| `Some(id)` | yes | `SelectedAccount` — that row's `username` and `profile_id` |
| `Some(id)` | no | `SelectedAccountMissing` — the offline profile, logged as a **warning**, because the player selected an account and is not getting it |
| `None` | — | `Offline` — `OfflineIdentity`'s name and its derived UUID |

`JoinIdentity::announce` logs the rung at `info` (the dangling-selection arm at
`warn`) with the username *and* the UUID, then yields the `LoginProfile`. Every
arm logs, including the ordinary ones: a session that honoured the selection and
one that silently fell back used to look identical from the outside, which is the
whole defect.

`NetClient::connect`, `open_singleplayer`, `open_to_lan` and `connect_online`
resolve it and pass the `LoginProfile` into `connect_impl`. `connect_as` — the
live-gate constructor — does not: it builds an `OfflineIdentity` from the exact
username the caller asked for, because a gate that joined as the developer's
selected account would make every gate share one premium player file.

### Identity is not authentication

This module answers only "what goes in the login-start packet".

*Authentication* is the other axis and it stays where it was:
`lodestone_auth::resolve_selected_account`, called on the net thread for a
`RemoteAuth::SelectedAccount` join. That one opens the OS keychain and POSTs the
refresh token to Microsoft, so it runs once per remote join and **never for
singleplayer** — vanilla skips the encryption request for a memory connection
(`ServerLoginPacketListenerImpl.handleHello` gates it on
`usesAuthentication() && !isMemoryConnection()`), so there is nothing to prove
and no session to spend.

The two cannot drift apart, because both key off the same
`AccountsMetadata::selected` UUID. Where authentication succeeds its session
profile wins, since it is the same account's name read fresher than
`profiles.json`'s copy.

### The UUID consequence

`CLAUDE.md` records that **vanilla** offline mode derives the account UUID from
the username and ignores the one the client sent. Our integrated server does
not — `lodestone-server`'s login handler echoes the presented UUID
(`login_uuid = Some(uuid)`) and keys the saved player file on it.

So the singleplayer path *is* affected by the identity change, in a way vanilla's
offline servers are not: a world entered before this change has its inventory and
position filed under the offline UUID, and entering it with an account selected
finds no save and starts that account fresh. Deselecting the account brings the
old player back. This is the same behaviour vanilla has when a world is opened by
a different account, and it is the price of honouring the selection at all — the
alternative is a client whose name, skin and save key disagree, which is where
this started.

## How to change it

* **Add a rung in `resolve`, never at a call site.** A second place that answers
  "who am I" is the defect this module replaced.
* **`join_identity()` forks on `#[cfg(test)]`** and always returns the offline
  identity under `cargo test`. That is not a convenience: a unit test that joined
  as whichever account the developer has selected would make every gate in the
  crate share one player file and would make the identity differ per machine — a
  test whose input is the person running it. The fork is asserted by
  `unit_tests_never_join_as_the_selected_account`, and the production decision is
  pinned separately by `a_selected_account_is_the_join_identity`, so the pair
  cannot both be satisfied by never resolving anything. Note the fork covers the
  *lib* test target only; an integration test under `tests/` links the production
  build and should use `NetClient::connect_as` if it cares about its identity.
* **Anything that falls back must say so.** `announce` is where that happens; a
  wrong-but-plausible username is the failure that looks like success.

## Configuration

None of its own. `LODESTONE_DATA_DIR` relocates both files it reads —
`profiles.json` (`lodestone_auth::paths::profiles_path`) and `offline.json`
(`offline_identity::offline_identity_path`).

## Dependencies

* `lodestone-auth` — `AccountsMetadata` only, which is a plain JSON read and is
  available on `wasm32` too (the sign-in chain is not).
* `crates/lodestone-shell/src/offline_identity.rs` — the fallback rung.
* `lodestone-model`'s `LoginProfile` — the value `crate::net` hands the client
  builder.

See also `docs/accounts.md` (the switcher and the online-mode handshake),
`docs/offline-identity.md` (the fallback's own file format and UUID derivation)
and `docs/player-skins.md` (the skin half, which resolves separately through the
tab-list ladder).
