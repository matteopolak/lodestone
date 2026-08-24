# Default-pack screenshot and coverage refresh design

## Goal

Regenerate every committed in-game image using Minecraft 26.2's built-in default resources, regardless of which local resource pack the developer has enabled, and refresh the current protocol-completeness/version-support figures from the repository's connectedness census.

## Root cause

`capture_screenshots.rs` deliberately builds the same production resource stack as the windowed client. On native builds that stack lazily reads `resource_packs.json`, so a locally selected pack such as Faithful 32x is layered above `client.jar` before the block, GUI, item, font, entity, and particle resources are built. The capture harness did not override that process-wide selection.

The README version-support table and the leading measured-coverage block in `docs/roadmap/protocol.md` are hand-copied snapshots. Their counts predate substantial adapter and server work; `cargo xtask connectedness` is the authoritative current source.

## Design

The capture binary will clear the in-process selected-pack order before constructing `Sim`. This changes only the capture process: it neither edits nor saves the player's persisted selection. Every production loader continues to run, but its stack contains only the built-in `client.jar` (and no local selected pack). A hermetic test will seed a fake selected pack, invoke the capture configuration boundary, and assert that the selection is empty before any atlas can be built.

All five files under `docs/images/` will then be regenerated in one `just screenshots` run. Visual review will check that each image uses the 16x vanilla art and that the existing scene-specific content remains visible.

The README and roadmap current-measurement block will be updated from one fresh `cargo xtask connectedness` run. Historical, explicitly dated measurements deeper in the roadmap remain unchanged because they describe earlier development states rather than current figures.

## Verification

- Red/green capture-selection regression test.
- Fresh `cargo xtask connectedness` output matches the two current snapshots.
- `just screenshots` renders all five scenes without `LODESTONE_SCENES` filtering.
- Visual inspection of all five 2560×1440 PNGs confirms vanilla 16x resources and intact scene content.
- Documentation index and focused shell tests pass before commit. Run full `just health`, classifying any parallel-only failure with an isolated single-threaded rerun, and require both wasm runners (including Trunk) to pass.

## Configuration and dependencies

`LODESTONE_SCENES` remains an optional iteration filter but is not used for the final refresh. The run still requires the flat creative oracle, a GPU adapter, and the cached 26.2 vanilla assets. No persisted user configuration is rewritten.
