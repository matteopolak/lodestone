#!/usr/bin/env python3
"""Stage a curated `.ogg` sound subset (plus the full event registry) for the
browser build.

What this is
-------------
`crates/lodestone-shell/src/audio.rs`'s wasm32 `ShellAudio` needs two things
neither `client.jar` nor a browser's filesystem-less runtime can give it:
`minecraft/sounds.json` (the event -> sample-list registry) and the `.ogg`
samples it indexes. Both live in the launcher's content-addressed
asset-object store (`objects/<hash[0:2]>/<hash>`, mapped from a logical name
by a local `asset-index-*.json`) -- the exact same store
`stage_panorama.py` already resolves the title-screen faces out of. See that
script's module doc and `crates/lodestone-shell/src/asset_objects.rs` for why
`client.jar` is not the whole pack.

**Never the full corpus.** `sounds.json` indexes 4871 `.ogg` objects / ~375 MB
(`docs/sound-playback.md`'s `xtask fetch-sounds` table) -- an unconditional
fetch would blow any reasonable page-load budget. This stages a small,
hand-curated *event* allowlist instead of a file list, for the same reason
`xtask::plan_sound_corpus` resolves from `sounds.json` rather than a hand-kept
name list: an event name is stable across a version bump, a resolved file
name (`step/stone3.ogg`) is an implementation detail of how many variants
that event happens to have this version.

The curation (measured on 26.2, this script's own resolution):
  * `ui.button.click` -- every menu button in the shell now plays this
    (`crate::sim::Sim::play_ui_click_sound`); with no browser audio at all,
    the owner's literal report ("clicking menu buttons ... doesnt play") was
    permanently silent, not fixable by any staged corpus, until this repo had
    a WebAudio backend at all.
  * `block.{stone,wood,grass}.{break,place,step}` -- the other half of that
    same report ("breaking blocks") plus footsteps, on the three material
    families a fresh world spends the most time on: underfoot stone/grass and
    built structures. Nine events.
  * `entity.{cow,pig,zombie}.ambient` -- one passive overworld mob, one more,
    one hostile, so a browser session's first few minutes are not silent the
    moment something living is nearby.
  * `entity.item.pickup`, `entity.experience_orb.pickup`, `entity.player.hurt`
    -- the three most common "something just happened to me" cues.

16 events resolving to 46 unique `.ogg` objects (some variants are shared
across events) -- measured 411,904 B raw / 375,502 B gzip -- plus the full
`sounds.json` (626,160 B raw / 44,671 B gzip, small enough relative to what it
indexes that `platform::assets::Bundle`'s own field doc already treats
shipping it whole as reasonable, same as `client.jar`/`blocks.json`). Total
measured: 1,038,064 B raw / 420,173 B gzip. This is fetched at *runtime*
(`web/src/main.rs`'s `fetch_sound_bundle`), the same seam `client.jar` and the
panorama faces already use -- it is not linked into the `.wasm` binary and so
does not count against `just wasm-size`'s enforced ceiling on the compiled
artifact, but the number is reported here because a real page load still pays
for it over the wire.

Extending the curation: add an event name to `CURATED_EVENTS` below. No other
file needs to change -- `web/src/main.rs` fetches whatever this script's
manifest lists, by name, and never hardcodes the set.

Fail-open, exactly like `stage_panorama.py`: a missing index, a missing
`objects/` tree, or an individual sample not yet downloaded is reported by a
named line on stdout and is NOT a build failure. `ShellAudio::from_env`
already degrades an empty/partial bundle to "audio disabled" or "audio
enabled, some events silent" with a logged reason -- see that function's own
docs -- so an unstaged corpus just means a quieter browser session, never a
broken one. This script always exits 0.

How to populate the source store: `cargo run -p xtask -- fetch-assets
--version <version>` (gets `sounds.json` itself) and `cargo run -p xtask --
fetch-sounds --version <version>` (gets the `.ogg` corpus this script picks
its subset from).
"""

import json
import shutil
import sys
from pathlib import Path
from typing import Optional

# Event names, not file names -- see the module doc for why. Each resolves to
# whatever `sounds.json` currently lists under it (1-6 variants, this
# version), walked exactly like `xtask::plan_sound_corpus` does: a bare
# string or a `{"type": "sound", "name": ...}` object contributes a sample; a
# `{"type": "event", "name": ...}` object is an indirection to another event,
# resolved recursively rather than staged itself.
CURATED_EVENTS = [
    "ui.button.click",
    "block.stone.break",
    "block.stone.place",
    "block.stone.step",
    "block.wood.break",
    "block.wood.place",
    "block.wood.step",
    "block.grass.break",
    "block.grass.place",
    "block.grass.step",
    "entity.cow.ambient",
    "entity.pig.ambient",
    "entity.zombie.ambient",
    "entity.item.pickup",
    "entity.experience_orb.pickup",
    "entity.player.hurt",
]

# Depth guard against a pathological `"type": "event"` cycle. No real 26.2
# event chains this deep -- it exists so a corrupt/hostile sounds.json cannot
# hang the build, not because any real chain needs it.
MAX_EVENT_INDIRECTION = 8


def sound_object_name(name: str) -> str:
    """Mirrors `xtask::sound_object_name` exactly: a name may carry its own
    `namespace:path`, in which case that namespace replaces `minecraft`."""
    if ":" in name:
        namespace, path = name.split(":", 1)
    else:
        namespace, path = "minecraft", name
    return f"{namespace}/sounds/{path}.ogg"


def resolve_event(events: dict, event: str, seen: set[str], depth: int) -> list[str]:
    """The object names (index keys) `event` resolves to, walking `"type":
    "event"` indirections. `seen` stops a cycle from recursing forever;
    `depth` is the belt-and-braces cap alongside it."""
    if event in seen or depth > MAX_EVENT_INDIRECTION:
        return []
    seen = seen | {event}
    definition = events.get(event)
    if not isinstance(definition, dict):
        return []
    entries = definition.get("sounds")
    if not isinstance(entries, list):
        return []
    out = []
    for entry in entries:
        if isinstance(entry, str):
            out.append(sound_object_name(entry))
        elif isinstance(entry, dict):
            kind = entry.get("type", "sound")
            name = entry.get("name")
            if not isinstance(name, str):
                continue
            if kind == "event":
                out.extend(resolve_event(events, name, seen, depth + 1))
            else:
                out.append(sound_object_name(name))
    return out


def find_asset_index(cache_dir: Path) -> Optional[Path]:
    """Identical discipline to `stage_panorama.py`'s own -- refuse to guess
    between several `asset-index-*.json` files rather than silently picking
    one, so this script and the native reader never disagree about which
    index is authoritative."""
    matches = sorted(cache_dir.glob("asset-index-*.json"))
    if len(matches) == 1:
        return matches[0]
    if len(matches) == 0:
        print(f"stage_sounds: no asset-index-*.json in {cache_dir}, staging nothing "
              f"(run: cargo run -p xtask -- fetch-assets --version <version>)")
    else:
        print(f"stage_sounds: {len(matches)} asset-index-*.json files in {cache_dir}; "
              f"refusing to guess, staging nothing")
    return None


def object_path(cache_dir: Path, object_hash: str) -> Path:
    return cache_dir / "objects" / object_hash[0:2] / object_hash


def main() -> int:
    if len(sys.argv) != 5 or sys.argv[1] != "--cache-dir" or sys.argv[3] != "--out":
        print("usage: stage_sounds.py --cache-dir <dir> --out <dir>", file=sys.stderr)
        return 0  # fail-open even on a malformed invocation from the hook

    cache_dir = Path(sys.argv[2])
    out_dir = Path(sys.argv[4])

    if not cache_dir.is_dir():
        print(f"stage_sounds: {cache_dir} is not a directory, staging nothing")
        return 0

    index_path = find_asset_index(cache_dir)
    if index_path is None:
        return 0

    try:
        index = json.loads(index_path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        print(f"stage_sounds: could not parse {index_path}: {e}, staging nothing")
        return 0

    objects = index.get("objects")
    if not isinstance(objects, dict):
        print(f"stage_sounds: {index_path} has no \"objects\" map, staging nothing")
        return 0

    sounds_meta = objects.get("minecraft/sounds.json")
    if not isinstance(sounds_meta, dict) or "hash" not in sounds_meta:
        print("stage_sounds: minecraft/sounds.json not in the asset index, staging nothing "
              "(run: cargo run -p xtask -- fetch-assets --version <version>)")
        return 0
    sounds_json_path = object_path(cache_dir, sounds_meta["hash"])
    if not sounds_json_path.is_file():
        print(f"stage_sounds: {sounds_json_path} absent, staging nothing "
              "(run: cargo run -p xtask -- fetch-assets --version <version>)")
        return 0
    try:
        sounds_json_bytes = sounds_json_path.read_bytes()
        events = json.loads(sounds_json_bytes)
    except (OSError, json.JSONDecodeError) as e:
        print(f"stage_sounds: could not parse {sounds_json_path}: {e}, staging nothing")
        return 0
    if not isinstance(events, dict):
        print("stage_sounds: minecraft/sounds.json is not an event object, staging nothing")
        return 0

    # The registry itself, staged flat at the page root -- same "small enough
    # to ship whole" call `client.jar`/`blocks.json` already make, and what
    # `crate::platform::assets::Bundle::sounds_json`'s own doc names as the
    # intended source.
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "sounds.json").write_bytes(sounds_json_bytes)
    print(f"stage_sounds: staged sounds.json ({len(sounds_json_bytes)} B)")

    wanted: list[str] = []
    for event in CURATED_EVENTS:
        if event not in events:
            print(f"stage_sounds: curated event {event!r} not in this version's "
                  f"sounds.json, skipping it")
            continue
        resolved = resolve_event(events, event, set(), 0)
        if not resolved:
            print(f"stage_sounds: curated event {event!r} resolved to no sample, skipping it")
        wanted.extend(resolved)
    # De-duplicate while keeping output deterministic — several curated events
    # share a variant pool in practice (none do on 26.2's own list above, but
    # nothing here assumes that stays true).
    wanted = sorted(set(wanted))

    sounds_out = out_dir / "sounds"
    staged: list[str] = []
    for name in wanted:
        meta = objects.get(name)
        if not isinstance(meta, dict) or "hash" not in meta:
            print(f"stage_sounds: {name} not in the asset index, skipping")
            continue
        object_hash = meta["hash"]
        declared_size = meta.get("size")
        src = object_path(cache_dir, object_hash)
        if not src.is_file():
            print(f"stage_sounds: {src} absent, skipping {name} "
                  "(run: cargo run -p xtask -- fetch-sounds --version <version>)")
            continue
        actual_size = src.stat().st_size
        if declared_size is not None and actual_size != declared_size:
            print(f"stage_sounds: {src} is {actual_size} B, index declares "
                  f"{declared_size} B; treating as absent rather than staging a "
                  f"truncated sample")
            continue
        dest = sounds_out / name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)
        staged.append(name)

    # The manifest is what `web/src/main.rs`'s `fetch_sound_bundle` reads to
    # know which of `CURATED_EVENTS`' resolved names actually made it to
    # disk here — the Rust side never hardcodes the file list, exactly so
    # editing `CURATED_EVENTS` above is the *only* place this corpus is
    # defined.
    manifest_path = sounds_out / "manifest.json"
    if staged:
        sounds_out.mkdir(parents=True, exist_ok=True)
        manifest_path.write_text(json.dumps(staged))

    print(f"stage_sounds: staged {len(staged)}/{len(wanted)} sound objects "
          f"for {len(CURATED_EVENTS)} curated events")
    return 0


if __name__ == "__main__":
    sys.exit(main())
