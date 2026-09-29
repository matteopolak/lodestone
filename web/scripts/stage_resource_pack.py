#!/usr/bin/env python3
"""Stage a visual pack over game definitions and default player skins.

The result contains Whimscape's art and overrides, plus the model, item, font,
language, recipe, and tag definitions it expects from the base game. The
default player skins are the sole base-game image exception.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import zipfile
from pathlib import Path
from typing import Any

ARCHIVE_NAME = "lodestone-resources.zip"
MANIFEST_NAME = "lodestone-resources.zip.manifest.json"
MANIFEST_VERSION = 1
INCLUDED_ROOTS = ("assets/",)
INCLUDED_DATA = ("recipe/", "tags/item/")
INCLUDED_FILES = (".mcassetsroot", "version.json", "pack.mcmeta", "pack.png")
PACK_DESCRIPTION = (
    "Whimscape by kavast\nhttps://www.curseforge.com/minecraft/texture-packs/whimscape"
)
PLAYER_SKIN_PREFIX = "assets/minecraft/textures/entity/player/"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar", type=Path, required=True)
    parser.add_argument("--visual-pack", type=Path, required=True)
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


def selected_entries(
    source: zipfile.ZipFile, *, definitions_only: bool, prefix: str = ""
) -> list[tuple[str, bytes]]:
    if source.testzip() is not None:
        raise ValueError("source archive failed its CRC check")
    entries: dict[str, zipfile.ZipInfo] = {}
    for info in source.infolist():
        if not info.filename.startswith(prefix):
            continue
        name = info.filename.removeprefix(prefix)
        if info.is_dir() or not safe_resource_name(name):
            continue
        player_skin = name.startswith(PLAYER_SKIN_PREFIX) and name.endswith(".png")
        if definitions_only and not player_skin and (
            "/textures/" in name
            or name in ("pack.mcmeta", "pack.png")
            or name.lower().endswith((".png", ".jpg", ".jpeg", ".tga", ".webp", ".gif"))
        ):
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


def format_number(value: Any) -> int:
    if type(value) is not int or not 0 <= value <= 0xFFFFFFFF:
        raise ValueError("resource format must be an unsigned 32-bit integer")
    return value


def format_range(metadata: dict[str, Any]) -> tuple[int, int]:
    if "min_format" in metadata or "max_format" in metadata:
        limits = (metadata.get("min_format"), metadata.get("max_format"))
    else:
        supported = metadata.get("supported_formats", metadata.get("pack_format"))
        if isinstance(supported, dict):
            limits = (supported.get("min_inclusive"), supported.get("max_inclusive"))
        elif isinstance(supported, list) and len(supported) == 2:
            limits = tuple(supported)
        else:
            limits = (supported, supported)
    low, high = (format_number(value) for value in limits)
    if low > high:
        raise ValueError("resource format range is reversed")
    return low, high


def stage(jar: Path, visual_pack: Path, out_dir: Path) -> dict[str, Any]:
    if not jar.is_file():
        raise ValueError(f"source archive is not a file: {jar}")
    if not visual_pack.is_file():
        raise ValueError(f"visual pack is not a file: {visual_pack}")
    try:
        with zipfile.ZipFile(jar) as source, zipfile.ZipFile(visual_pack) as visual:
            entries = dict(selected_entries(source, definitions_only=True))
            visual_entries = dict(selected_entries(visual, definitions_only=False))
            if "pack.mcmeta" not in visual_entries:
                raise ValueError("visual pack has no pack.mcmeta")
            if not any("/textures/" in name for name in visual_entries):
                raise ValueError("visual pack has no textures")
            metadata = json.loads(visual_entries["pack.mcmeta"])
            if not isinstance(metadata, dict) or not isinstance(metadata.get("pack"), dict):
                raise ValueError("visual pack metadata has no pack object")
            try:
                version = json.loads(entries["version.json"])
                host_format = format_number(version["pack_version"]["resource_major"])
            except (KeyError, TypeError) as error:
                raise ValueError("source version.json has no resource pack format") from error
            low, high = format_range(metadata["pack"])
            if not low <= host_format <= high:
                raise ValueError(f"visual pack formats {low}..{high} exclude host format {host_format}")
            overlays = metadata.pop("overlays", {})
            if not isinstance(overlays, dict) or not isinstance(overlays.get("entries", []), list):
                raise ValueError("visual pack overlays must contain an entries array")
            for overlay in overlays.get("entries", []):
                if not isinstance(overlay, dict):
                    raise ValueError("visual pack overlay must be an object")
                low, high = format_range(overlay)
                directory = overlay.get("directory")
                if not isinstance(directory, str) or not directory or any(
                    part in ("", ".", "..") for part in directory.split("/")
                ):
                    raise ValueError("visual pack overlay has an invalid directory")
                if low <= host_format <= high:
                    visual_entries.update(selected_entries(
                        visual, definitions_only=False, prefix=directory + "/"
                    ))
            metadata["pack"].update({
                "description": PACK_DESCRIPTION,
                "pack_format": host_format,
                "supported_formats": [host_format, host_format],
                "min_format": host_format,
                "max_format": host_format,
            })
            visual_entries["pack.mcmeta"] = (
                json.dumps(metadata, sort_keys=True, separators=(",", ":")) + "\n"
            ).encode("utf-8")
            entries.update(visual_entries)
    except (OSError, zipfile.BadZipFile) as error:
        raise ValueError(f"could not open source archive: {error}") from error

    out_dir.mkdir(parents=True, exist_ok=True)
    archive_path = out_dir / ARCHIVE_NAME
    with zipfile.ZipFile(
        archive_path,
        mode="w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
        allowZip64=True,
    ) as target:
        for name, data in sorted(entries.items()):
            target.writestr(deterministic_info(name), data, compress_type=zipfile.ZIP_DEFLATED, compresslevel=9)

    archive_bytes = archive_path.read_bytes()
    manifest = {
        "version": MANIFEST_VERSION,
        "asset": ARCHIVE_NAME,
        "bytes": len(archive_bytes),
        "sha256": hashlib.sha256(archive_bytes).hexdigest(),
        "entries": len(entries),
        "roots": ["assets/", "data/*/recipe/", "data/*/tags/item/"],
        "metadata": [name for name in entries if name in INCLUDED_FILES],
    }
    (out_dir / MANIFEST_NAME).write_text(
        json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    return manifest


def main() -> int:
    args = parse_args()
    try:
        manifest = stage(args.jar, args.visual_pack, args.out)
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
