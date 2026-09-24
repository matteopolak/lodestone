# Anvil player locator import

## What it is

`lodestone_server::anvil_player_storage` imports an explicitly selected deterministic batch of gzip-wrapped Anvil player-data files into a small typed native player record. It retains identity, dimension, position, rotation, and game mode; the Anvil player file remains the complete player-state authority.

## How it works

`preflight_player_file` reads the selected file through `lodestone_anvil::player_dat`, decodes its schema with `PlayerData::from_nbt`, and returns a payload-free `PlayerImportReport`. The report always names the source data version and full-player values the native record cannot retain: motion, vital state, fall distance, ground state, inventory selection and contents, and experience. It additionally reports preserved root fields and any position or rotation rounding. An absent source game mode stays absent in the typed record; older locator-only native records also reopen with no game mode.

`import_player_file` reruns that preflight and accepts only the exact matching `PlayerImportAuthorization` made with `PlayerLossDecision::ProceedAndDiscardUnsupported`. Missing, aborted, blocked, and stale authorizations all fail before a native write. The UUID is supplied separately because the player root does not contain the filename identity that owns the native compact key. Missing source files return `Ok(None)`, matching first-join semantics.

`discover_player_files` selects either explicitly named UUIDs or every canonical UUID `.dat` file under `players/data`, always in UUID order. It rejects malformed `.dat` filenames rather than silently omitting a save. `preflight_player_batch` decodes every selected file before a native backend is opened and keeps only typed player values for `import_player_batch`; the aggregate report has no unsupported NBT payload. The batch validates every complete UUID and compact-key collision before `WorldStorage::write_dirty_player_data_batch` commits the records in one native transaction.

`lodestone-server anvil-convert import-players` is the reviewed filesystem caller. It requires `--source`, `--destination`, an identical `--native-path`, and exactly one selection mode: repeated `--player <uuid>` or `--all-players`. Preview reports the deterministic selected count and every payload-free loss category without creating the native destination. `--apply` needs the exact printed `--acknowledge` token whenever any selected player is lossy; blockers cannot be acknowledged. After committing, the command closes and reopens the native backend and reads every selected UUID. It imports only the typed partial record and never rewrites source player data.

The importer uses a declared producer contract of 1,000 fixed position units per block and native millidegrees for yaw and pitch. A custom dimension, non-finite pose, or rounded value outside a signed `i32` blocks import; ordinary fractional precision is a reported loss that needs authorization. The resulting `NativePlayerData` is written through `WorldStorage::write_dirty_player_data`, whose existing collision checks still protect complete UUID identity. Game mode uses the schema enum rather than an Anvil ordinal; unknown stored enum values fail closed.

## How to change it

Add a preflight entry before omitting any additional field from `PlayerData`. Do not turn a player root into an opaque extension: the typed native record has no consumer for complete inventory or preserved NBT. If another producer needs a different coordinate unit, give it a separate contract and reader; this schema intentionally does not carry a scale marker, so silently sharing records across units would move players.

Keep `import_player_file` on `lodestone_anvil::player_dat` rather than duplicating gzip or file-path handling. Keep batch decoding ahead of `WorldStorage::write_dirty_player_data_batch`: opening the native backend or writing a first player before the last source file is safe would violate the review boundary. Do not add player export until there is a full typed player-state destination; exporting this partial record as a player file would manufacture inventory, health, velocity, and preserved fields the format does not retain. The fixture test names expected native values directly after decoding a separately encoded Anvil fixture; preserve that separation instead of replacing it with a converter round trip.

## Configuration

There are no environment variables. Library callers select source paths, UUIDs, and the native `WorldStorage` backend. The operator command uses `import-players --source <anvil-world> --destination <native-store> --native-path <native-store> (--player <uuid> ... | --all-players)`, optionally followed by `--apply --acknowledge <review-token>`. This importer writes milliblock-position records plus an optional typed game mode only.

## Dependencies

The module depends on `lodestone-anvil` for the player-data container, `player_data::PlayerData` for the supported 26.2 schema, `world_storage::NativePlayerData` for the typed destination, and `uuid` for the filename identity supplied by the caller.
