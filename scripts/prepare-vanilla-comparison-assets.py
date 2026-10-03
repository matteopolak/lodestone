#!/usr/bin/env python3
"""Stage unchanged cached game resources for an isolated local comparison."""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import shutil
import sys
import zipfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
BENCHMARK_ROOT = ROOT / ".cache" / "benchmarks"
MANIFEST_NAME = "vanilla-comparison-assets.json"


def file_identity(path: pathlib.Path) -> dict:
    sha1 = hashlib.sha1()
    sha256 = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            size += len(chunk)
            sha1.update(chunk)
            sha256.update(chunk)
    return {"bytes": size, "sha1": sha1.hexdigest(), "sha256": sha256.hexdigest()}


def inspect_archive(path: pathlib.Path, release: str) -> list[dict]:
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError("source archive contains duplicate entries")
        try:
            version = archive.getinfo("version.json")
        except KeyError as error:
            raise ValueError("source archive has no version.json") from error
        if version.file_size > 1024 * 1024:
            raise ValueError("source archive version.json exceeds 1 MiB")
        metadata = json.loads(archive.read(version))
        if not isinstance(metadata, dict) or metadata.get("id") != release:
            raise ValueError(f"source archive release does not match {release!r}")
        bad_entry = archive.testzip()
        if bad_entry is not None:
            raise ValueError(f"source archive failed CRC validation: {bad_entry}")
        textures = []
        for info in sorted(archive.infolist(), key=lambda entry: entry.filename):
            if info.is_dir() or not (info.filename.startswith("assets/")
                    and "/textures/" in info.filename and info.filename.endswith(".png")):
                continue
            digest = hashlib.sha256()
            with archive.open(info) as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
            textures.append({"path": info.filename, "bytes": info.file_size, "sha256": digest.hexdigest()})
        if not textures:
            raise ValueError("source archive contains no texture PNGs")
        return textures


def stage_assets(
    client_jar: pathlib.Path,
    blocks_json: pathlib.Path,
    release: str,
    output: pathlib.Path,
    expected_sha1: str | None = None,
) -> dict:
    """Create one new benchmark root; preserve source bytes and refuse replacement."""
    if not release.strip():
        raise ValueError("release must be nonblank")
    if expected_sha1 is not None and not re.fullmatch(r"[0-9a-f]{40}", expected_sha1):
        raise ValueError("expected SHA-1 must be 40 lowercase hexadecimal digits")
    output = output.absolute()
    isolated_root = BENCHMARK_ROOT.resolve()
    resolved = output.resolve()
    if not resolved.is_relative_to(isolated_root) or resolved == isolated_root:
        raise ValueError(f"output must be a new isolated directory below {BENCHMARK_ROOT}")
    if output.exists() or output.is_symlink():
        raise FileExistsError(f"refusing to overwrite existing asset output: {output}")
    client_jar = client_jar.resolve(strict=True)
    blocks_json = blocks_json.resolve(strict=True)
    if not client_jar.is_file() or not blocks_json.is_file():
        raise ValueError("source jar and block report must be regular files")
    before = client_jar.stat()
    textures = inspect_archive(client_jar, release)
    jar_identity = file_identity(client_jar)
    after = client_jar.stat()
    if (before.st_size, before.st_mtime_ns, before.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino):
        raise ValueError("source jar changed during inspection")
    if expected_sha1 is not None and jar_identity["sha1"] != expected_sha1:
        raise ValueError(f"source jar SHA-1 mismatch: expected {expected_sha1}, got {jar_identity['sha1']}")
    report_before = blocks_json.stat()
    with blocks_json.open(encoding="utf-8") as handle:
        report = json.load(handle)
    if not isinstance(report, dict) or not report:
        raise ValueError("block report must be a nonempty JSON object")
    report_identity = file_identity(blocks_json)
    report_after = blocks_json.stat()
    if (report_before.st_size, report_before.st_mtime_ns, report_before.st_ino) != (report_after.st_size, report_after.st_mtime_ns, report_after.st_ino):
        raise ValueError("block report changed during inspection")
    inventory_bytes = json.dumps(textures, sort_keys=True, separators=(",", ":")).encode("utf-8")
    existing_parent = output.parent
    while not existing_parent.exists():
        existing_parent = existing_parent.parent
    required_bytes = jar_identity["bytes"] + report_identity["bytes"] + len(inventory_bytes) + 1024 * 1024
    if shutil.disk_usage(existing_parent).free < required_bytes:
        raise OSError(f"insufficient free space for isolated asset copies: need {required_bytes} bytes")
    output.mkdir(parents=True, exist_ok=False)
    assets = {}
    for source, name, identity in (
        (client_jar, "lodestone-resources.zip", jar_identity),
        (blocks_json, "generated/reports/blocks.json", report_identity),
    ):
        destination = output / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        with source.open("rb") as reader, destination.open("xb") as writer:
            shutil.copyfileobj(reader, writer, length=1024 * 1024)
        if file_identity(destination) != identity:
            raise ValueError(f"source changed or copy failed: {source}; partial output retained at {output}")
        assets[name] = identity
    manifest = {
        "schema": 1, "purpose": "local vanilla comparison", "release": release,
        "source_jar": {"path": str(client_jar), **jar_identity, "version_id": release,
                       "expected_sha1": expected_sha1, "expected_sha1_matched": expected_sha1 is not None},
        "source_blocks_json": {"path": str(blocks_json), **report_identity},
        "assets": assets,
        "texture_inventory": {"count": len(textures), "entries": textures,
                              "sha256": hashlib.sha256(inventory_bytes).hexdigest()},
        "official_source_authenticity_verified": False,
        "block_report_release_verified": False,
        "full_source_archive_preserved": True,
        "external_indexed_assets_included": False,
    }
    with (output / MANIFEST_NAME).open("x", encoding="utf-8") as handle:
        handle.write(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar", type=pathlib.Path, required=True)
    parser.add_argument("--blocks-json", type=pathlib.Path, required=True)
    parser.add_argument("--release", required=True)
    parser.add_argument("--out", type=pathlib.Path, required=True)
    parser.add_argument("--expected-sha1", help="independently supplied expected source-jar SHA-1")
    args = parser.parse_args(argv)
    manifest = stage_assets(args.jar, args.blocks_json, args.release, args.out, args.expected_sha1)
    print(f"staged {manifest['texture_inventory']['count']} unchanged texture PNGs in full archive: {args.out}")
    print(f"resource SHA-256: {manifest['source_jar']['sha256']}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, zipfile.BadZipFile, RuntimeError) as error:
        print(f"asset staging failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error
