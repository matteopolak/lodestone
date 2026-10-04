#!/usr/bin/env python3
"""Split a browser `lodestone-resources.zip` into deterministic, host-safe parts.

Hosts with per-file limits can receive `lodestone-resources.zip.parts.json` and
ordered sibling parts, each at most 20 MiB, for `web/src/resource_pack.rs` to
validate and join in the browser. Names are plain filenames and the page
resolves them below its own deployment base path.

Usage:
    python3 web/scripts/stage_resource_pack_parts.py \
      --jar .cache/mc/<version>/lodestone-resources.zip --out web/dist

The output is deterministic for identical input bytes: content-addressed
filenames, fixed part size, sorted-key compact JSON, and SHA-256 digests. This writes only the
manifest and its named part files; callers package the output directory and may
remove a copied direct `lodestone-resources.zip` for hosts with a per-file cap.
"""

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any

PART_BYTES = 20 * 1024 * 1024
MANIFEST_NAME = "lodestone-resources.zip.parts.json"
PART_PREFIX = "lodestone-resources.zip.part-"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar", type=Path, required=True, help="source lodestone-resources.zip")
    parser.add_argument("--out", type=Path, required=True, help="directory to receive parts")
    parser.add_argument(
        "--part-bytes",
        type=int,
        default=PART_BYTES,
        help=f"part size in bytes (1..={PART_BYTES}; default: {PART_BYTES})",
    )
    args = parser.parse_args()
    if not 0 < args.part_bytes <= PART_BYTES:
        parser.error(f"--part-bytes must be between 1 and {PART_BYTES}")
    return args


def part_name(index: int, digest: str) -> str:
    return f"{PART_PREFIX}{index:03}-{digest}"


def stage(jar: Path, out_dir: Path, part_bytes: int) -> dict[str, Any]:
    if not 0 < part_bytes <= PART_BYTES:
        raise ValueError(f"part size must be between 1 and {PART_BYTES}")
    if not jar.is_file():
        raise ValueError(f"source resource archive is not a file: {jar}")
    out_dir.mkdir(parents=True, exist_ok=True)

    whole_hash = hashlib.sha256()
    parts: list[dict[str, Any]] = []
    total_bytes = 0
    with jar.open("rb") as source:
        index = 0
        while chunk := source.read(part_bytes):
            part_hash = hashlib.sha256(chunk).hexdigest()
            name = part_name(index, part_hash)
            (out_dir / name).write_bytes(chunk)
            parts.append({"name": name, "bytes": len(chunk), "sha256": part_hash})
            whole_hash.update(chunk)
            total_bytes += len(chunk)
            index += 1

    if not parts:
        raise ValueError("source resource archive is empty")
    return {
        "version": 1,
        "asset": "lodestone-resources.zip",
        "total_bytes": total_bytes,
        "sha256": whole_hash.hexdigest(),
        "parts": parts,
    }


def main() -> int:
    args = parse_args()
    try:
        manifest = stage(args.jar, args.out, args.part_bytes)
    except (OSError, ValueError) as error:
        print(f"stage_resource_pack_parts: {error}")
        return 1

    manifest_path = args.out / MANIFEST_NAME
    manifest_path.write_text(
        json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    print(
        f"stage_resource_pack_parts: staged {len(manifest['parts'])} part(s), "
        f"{manifest['total_bytes']} bytes, manifest {manifest_path}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
