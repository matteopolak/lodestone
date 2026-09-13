#!/usr/bin/env python3
"""Stage the six real title-screen panorama faces for the browser build.

What this is
-------------
`client.jar` ships only a 69-byte 1x1 grey stub for each of the six panorama
faces (`textures/gui/title/background/panorama_{0..5}.png`); the real
1024x1024 art lives in the launcher's asset-object store instead -- a
content-addressed tree (`objects/<hash[0:2]>/<hash>`) that a local
`asset-index-*.json` maps logical names onto. See
`crates/lodestone-shell/src/asset_objects.rs`'s module doc for the full
measurement (it is exactly 8 names, of which these 6 are the panorama).

Native reads that store directly (`AssetObjectStore::open`, plain
`std::fs`). A browser has no filesystem, so `web/`'s trunk build resolves the
six objects here, at BUILD time, into plain flat filenames
(`panorama_0.png` .. `panorama_5.png`) staged beside the page -- `web/`'s
`fetch_panorama_faces` then fetches whichever exist over HTTP, exactly as it
already does for `client.jar`/`blocks.json`.

Fail-open, like `Trunk.toml`'s existing client.jar/blocks.json hook: a
missing index, a missing `objects/` tree, or an individual face not yet
downloaded is reported by a named line on stdout and is NOT a build failure
-- `crate::menu::panorama::load` already falls back to client.jar's stub per
face, so an unstaged face just means a flatter title screen, never a broken
one. This script always exits 0.

How to populate the source store: `cargo run -p xtask -- fetch-assets
--version <version>`, which fetches the asset index plus exactly these 8
jar-shadowed objects (~2.6 MB), not the full ~5000-object index.
"""

import json
import shutil
import sys
from pathlib import Path
from typing import Optional

FACE_COUNT = 6
NAME_TEMPLATE = "minecraft/textures/gui/title/background/panorama_{n}.png"


def find_asset_index(cache_dir: Path) -> Optional[Path]:
    """The single `asset-index-*.json` in `cache_dir`, refusing to guess
    between several -- mirrors `asset_objects::find_asset_index`'s discipline
    exactly, so this script and the native reader never disagree about which
    index is authoritative."""
    matches = sorted(cache_dir.glob("asset-index-*.json"))
    if len(matches) == 1:
        return matches[0]
    if len(matches) == 0:
        print(f"stage_panorama: no asset-index-*.json in {cache_dir}, staging nothing "
              f"(run: cargo run -p xtask -- fetch-assets --version <version>)")
    else:
        print(f"stage_panorama: {len(matches)} asset-index-*.json files in {cache_dir}; "
              f"refusing to guess, staging nothing")
    return None


def main() -> int:
    if len(sys.argv) != 5 or sys.argv[1] != "--cache-dir" or sys.argv[3] != "--out":
        print("usage: stage_panorama.py --cache-dir <dir> --out <dir>", file=sys.stderr)
        return 0  # fail-open even on a malformed invocation from the hook

    cache_dir = Path(sys.argv[2])
    out_dir = Path(sys.argv[4])

    if not cache_dir.is_dir():
        print(f"stage_panorama: {cache_dir} is not a directory, staging nothing")
        return 0

    index_path = find_asset_index(cache_dir)
    if index_path is None:
        return 0

    try:
        index = json.loads(index_path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        print(f"stage_panorama: could not parse {index_path}: {e}, staging nothing")
        return 0

    objects = index.get("objects")
    if not isinstance(objects, dict):
        print(f"stage_panorama: {index_path} has no \"objects\" map, staging nothing")
        return 0

    staged = 0
    for n in range(FACE_COUNT):
        name = NAME_TEMPLATE.format(n=n)
        meta = objects.get(name)
        if not isinstance(meta, dict) or "hash" not in meta:
            print(f"stage_panorama: {name} not in the asset index, skipping")
            continue
        object_hash = meta["hash"]
        declared_size = meta.get("size")
        object_path = cache_dir / "objects" / object_hash[0:2] / object_hash
        if not object_path.is_file():
            print(f"stage_panorama: {object_path} absent, skipping panorama_{n}.png "
                  f"(run: cargo run -p xtask -- fetch-assets --version <version>)")
            continue
        actual_size = object_path.stat().st_size
        if declared_size is not None and actual_size != declared_size:
            print(f"stage_panorama: {object_path} is {actual_size} B, index declares "
                  f"{declared_size} B; treating as absent rather than staging a "
                  f"truncated face")
            continue
        out_dir.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(object_path, out_dir / f"panorama_{n}.png")
        staged += 1

    print(f"stage_panorama: staged {staged}/{FACE_COUNT} real panorama faces")
    return 0


if __name__ == "__main__":
    sys.exit(main())
