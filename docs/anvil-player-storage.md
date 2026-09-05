# Anvil player locator import

## What it is

`lodestone_server::anvil_player_storage` imports one explicitly selected gzip-wrapped Anvil player-data file into the much smaller `NativePlayerRecord` locator. It is a migration aid for identity, dimension, position, and rotation only; the Anvil player file remains the complete player-state authority.

## How it works

`preflight_player_file` reads the selected file through `lodestone_anvil::player_dat`, decodes its schema with `PlayerData::from_nbt`, and returns a payload-free `PlayerImportReport`. The report always names the source data version and full-player values the locator cannot retain: motion, vital state, fall distance, ground state, game mode, inventory selection and contents, and experience. It additionally reports preserved root fields and any position or rotation rounding.

`import_player_file` reruns that preflight and accepts only the exact matching `PlayerImportAuthorization` made with `PlayerLossDecision::ProceedAndDiscardUnsupported`. Missing, aborted, blocked, and stale authorizations all fail before a native write. The UUID is supplied separately because the player root does not contain the filename identity that owns the native compact key. Missing source files return `Ok(None)`, matching first-join semantics.

The importer uses a declared producer contract of 1,000 fixed position units per block and native millidegrees for yaw and pitch. A custom dimension, non-finite pose, or rounded value outside a signed `i32` blocks import; ordinary fractional precision is a reported loss that needs authorization. The resulting `NativePlayerRecord` is written through `WorldStorage::write_dirty_player`, whose existing collision checks still protect complete UUID identity.

## How to change it

Add a preflight entry before omitting any additional field from `PlayerData`. Do not turn a player root into an opaque extension: the native locator has no consumer for complete inventory or preserved NBT. If another producer needs a different coordinate unit, give it a separate contract and reader; this schema intentionally does not carry a scale marker, so silently sharing records across units would move players.

Keep `import_player_file` on `lodestone_anvil::player_dat` rather than duplicating gzip or file-path handling. The fixture test names expected native values directly after decoding a separately encoded Anvil fixture; preserve that separation instead of replacing it with a converter round trip.

## Configuration

There are no flags or environment variables. Callers select the source path, UUID, and native `WorldStorage` backend. This importer writes milliblock-position locator records only.

## Dependencies

The module depends on `lodestone-anvil` for the player-data container, `player_data::PlayerData` for the supported 26.2 schema, `world_storage::NativePlayerRecord` for the typed destination, and `uuid` for the filename identity supplied by the caller.
