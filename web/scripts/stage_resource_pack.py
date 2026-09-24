#!/usr/bin/env python3
"""Stage the browser's complete renderable resource pack from a client archive.

The runtime reads every file in ``assets/`` and the recipe/tag data below
``data/``.  JVM classes, signatures, reports, and launcher metadata are not
resources the browser loader can read, so retaining them only increases the
first fetch.  The output is a deterministic ZIP plus a digest manifest.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import zipfile
from pathlib import Path
from typing import Any

MANIFEST_NAME = "client.jar.manifest.json"
MANIFEST_VERSION = 1
INCLUDED_ROOTS = ("assets/",)
INCLUDED_DATA = ("recipe/", "tags/item/")
INCLUDED_FILES = (".mcassetsroot", "pack.mcmeta", "pack.png")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    return parser.parse_args()


def safe_resource_name(name: str) -> bool:
    if not name or "\0" in name or name.endswith("/"):
        return False
    if not (name.startswith(INCLUDED_ROOTS) or name in INCLUDED_FILES):
        if not name.startswith("data/"):
            return False
        data_path = name.removeprefix("data/").split("/", 1)
        if len(data_path) != 2 or not any(
            data_path[1].startswith(prefix) for prefix in INCLUDED_DATA
        ):
            return False
    parts = name.split("/")
    return all(part not in ("", ".", "..") for part in parts)


def deterministic_info(name: str) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = 3
    info.external_attr = 0o600 << 16
    info.comment = b""
    info.extra = b""
    return info


def selected_entries(source: zipfile.ZipFile) -> list[tuple[str, bytes]]:
    if source.testzip() is not None:
        raise ValueError("source archive failed its CRC check")
    entries: dict[str, zipfile.ZipInfo] = {}
    for info in source.infolist():
        name = info.filename
        if info.is_dir() or not safe_resource_name(name):
            continue
        entries[name] = info
    selected: list[tuple[str, bytes]] = []
    for name in sorted(entries):
        try:
            selected.append((name, source.read(entries[name])))
        except (RuntimeError, OSError, zipfile.BadZipFile) as error:
            raise ValueError(f"could not read resource {name}: {error}") from error
    if not selected:
        raise ValueError("source archive has no browser resources")
    return selected


def stage(jar: Path, out_dir: Path) -> dict[str, Any]:
    if not jar.is_file():
        raise ValueError(f"source archive is not a file: {jar}")
    try:
        with zipfile.ZipFile(jar) as source:
            entries = selected_entries(source)
    except (OSError, zipfile.BadZipFile) as error:
        raise ValueError(f"could not open source archive: {error}") from error

    out_dir.mkdir(parents=True, exist_ok=True)
    archive_path = out_dir / "client.jar"
    with zipfile.ZipFile(
        archive_path,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        allowZip64=True,
    ) as target:
        for name, data in entries:
            target.writestr(deterministic_info(name), data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)

    archive_bytes = archive_path.read_bytes()
    manifest = {
        "version": MANIFEST_VERSION,
        "asset": "client.jar",
        "bytes": len(archive_bytes),
        "sha256": hashlib.sha256(archive_bytes).hexdigest(),
        "entries": len(entries),
        "roots": ["assets/", "data/*/recipe/", "data/*/tags/item/"],
        "metadata": [name for name, _ in entries if name in INCLUDED_FILES],
    }
    (out_dir / MANIFEST_NAME).write_text(
        json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    return manifest


def main() -> int:
    args = parse_args()
    try:
        manifest = stage(args.jar, args.out)
    except ValueError as error:
        print(f"stage_resource_pack: {error}")
        return 1
    print(
        f"stage_resource_pack: staged {manifest['entries']} entries, "
        f"{manifest['bytes']} bytes, digest {manifest['sha256']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
