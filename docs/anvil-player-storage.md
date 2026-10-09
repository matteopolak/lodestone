# Anvil player locator import

## What it is

`lodestone_server::anvil_player_storage` imports an explicitly selected, deterministic batch of gzip-wrapped Anvil player-data files into a small typed native player record holding identity, dimension, position, rotation and game mode. The Anvil player file remains the complete player-state authority.

## How it works

- `preflight_player_file` reads the file through `lodestone_anvil::player_dat`, decodes with `PlayerData::from_nbt` and returns a payload-free `PlayerImportReport`. It always names the source data version and the full-player values the native record cannot keep (motion, vitals, fall distance, ground state, inventory selection and contents, experience), plus preserved root fields and any position or rotation rounding. An absent game mode stays absent; older locator-only records also reopen with none.
- `import_player_file` reruns preflight and accepts only the exact matching `PlayerImportAuthorization` made with `PlayerLossDecision::ProceedAndDiscardUnsupported`; missing, aborted, blocked and stale authorizations fail before any write. The UUID is supplied separately (the root lacks the filename identity that owns the native compact key). A missing file returns `Ok(None)`, matching first-join semantics.
- `discover_player_files` selects named UUIDs or every canonical UUID `.dat` under `players/data`, always in UUID order, rejecting malformed `.dat` names rather than omitting a save. `preflight_player_batch` decodes every selected file before a native backend opens and keeps only typed values for `import_player_batch`; it validates every UUID and compact-key collision, then `WorldStorage::write_dirty_player_data_batch` commits in one native transaction.
- Units: 1,000 fixed position units per block and native millidegrees for yaw and pitch. A custom dimension, non-finite pose or a rounded value outside signed `i32` blocks import; ordinary fractional rounding is a reported loss needing authorization. `NativePlayerData` goes through `WorldStorage::write_dirty_player_data` (collision checks still protect full UUID identity). Game mode uses the schema enum, not an Anvil ordinal, and unknown stored values fail closed.

## How to change it

- Add a preflight entry before omitting any further `PlayerData` field. Do not make the player root an opaque extension; the native record has no consumer for full inventory or preserved NBT. A producer with a different coordinate unit needs its own contract and reader, since this schema carries no scale marker.
- Keep `import_player_file` on `lodestone_anvil::player_dat` and batch decoding ahead of the batch write (opening the backend or writing a first player before the last source is verified breaks the review boundary). No player export until a full typed destination exists, because exporting this partial record would manufacture inventory, health, velocity and preserved fields. Fixture tests name expected native values after decoding a separately encoded Anvil fixture; do not replace that with a converter round trip.

## Configuration

No environment variables. Operator command:

```sh
lodestone-server anvil-convert import-players --source <anvil-world> --destination <native-store> \
  --native-path <native-store> (--player <uuid> ... | --all-players) [--apply --acknowledge <review-token>]
```

Exactly one selection mode is required. Preview reports the deterministic count and payload-free loss categories without creating the destination; `--apply` needs the exact printed token if any selected player is lossy (blockers cannot be acknowledged). After committing it closes and reopens the backend and reads every selected UUID. Source player data is never rewritten.

## Dependencies

`lodestone-anvil` (container), `player_data::PlayerData` (the supported 26.2 schema), `world_storage::NativePlayerData` and `uuid`.
