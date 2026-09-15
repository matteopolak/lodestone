#!/usr/bin/env python3
"""Build and package the embeddable Lodestone browser module."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / "web"
DEFAULT_OUTPUT = ROOT / "target" / "wasm-sdk"
ARCHIVE_NAME = "lodestone-web-sdk.tar.gz"
MANIFEST_NAME = "lodestone-web-sdk.manifest.json"
PANORAMA_FILES = tuple(f"panorama_{index}.png" for index in range(6))
REQUIRED_FILES = (
    "lodestone-web-entry.js",
    "lodestone-web-entry_bg.wasm",
    "lodestone-render-worker.js",
    "lodestone-server-worker.js",
    "lodestone-server-worker-bootstrap.js",
    "lodestone-server-worker-wasm-serial.js",
    "lodestone-server-worker-wasm-serial_bg.wasm",
    "lodestone-server-worker-wasm-threaded.js",
    "lodestone-server-worker-wasm-threaded_bg.wasm",
    "client.jar",
    "blocks.json",
    *PANORAMA_FILES,
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git_metadata(allow_dirty: bool) -> tuple[str, bool]:
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=ROOT, text=True
    ).strip()
    dirty = bool(
        subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip()
    )
    if dirty and not allow_dirty:
        raise SystemExit(
            "refusing to package a dirty checkout; commit the inputs or pass --allow-dirty"
        )
    return commit, dirty


def run_trunk(stage: Path) -> None:
    trunk = os.environ.get("LODESTONE_TRUNK", "trunk")
    command = [
        trunk,
        "build",
        "--release",
        "--locked",
        "--filehash=true",
        "--public-url=./",
        "--dist",
        str(stage),
    ]
    environment = os.environ.copy()
    environment.pop("NO_COLOR", None)
    subprocess.run(command, cwd=WEB, check=True, env=environment)


def copy_file(stage: Path, package: Path, relative: str) -> None:
    source = stage / relative
    if not source.is_file():
        raise SystemExit(f"required SDK artifact is missing: {relative}")
    destination = package / relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def collect_package(stage: Path, package: Path) -> tuple[list[str], str]:
    page_modules = sorted(
        path
        for path in stage.glob("lodestone-web-*.js")
        if path.name != "lodestone-web-entry.js"
    )
    if len(page_modules) != 1:
        raise SystemExit("expected exactly one hashed Lodestone page ESM module")
    page_module = page_modules[0]
    page_wasm = stage / f"{page_module.stem}_bg.wasm"
    if not page_wasm.is_file():
        raise SystemExit(f"page module Wasm is missing: {page_wasm.name}")
    copy_file(stage, package, page_module.name)
    copy_file(stage, package, page_wasm.name)
    for relative in REQUIRED_FILES:
        copy_file(stage, package, relative)

    for relative in (
        "lodestone-server-worker-wasm-serial.d.ts",
        "lodestone-server-worker-wasm-serial_bg.wasm.d.ts",
        "lodestone-server-worker-wasm-threaded.d.ts",
        "lodestone-server-worker-wasm-threaded_bg.wasm.d.ts",
        "client.jar.manifest.json",
    ):
        if (stage / relative).is_file():
            copy_file(stage, package, relative)

    snippets = stage / "snippets"
    snippet_files = sorted(path for path in snippets.rglob("*") if path.is_file()) if snippets.is_dir() else []
    if not snippet_files:
        raise SystemExit("required wasm-bindgen snippets are missing")
    for source in snippet_files:
        copy_file(stage, package, source.relative_to(stage).as_posix())

    if not any(path.name == "workerHelpers.no-bundler.js" for path in snippet_files):
        raise SystemExit("required threaded-worker snippet is missing")

    files = sorted(path.relative_to(package).as_posix() for path in package.rglob("*") if path.is_file())
    forbidden = {"index.html", "lodestone-worldgen-long-task-harness.js"}
    if forbidden.intersection(files):
        raise SystemExit("consumer page or diagnostic harness leaked into SDK package")
    return files, page_module.name


def write_archive(package: Path, archive: Path, files: list[str]) -> None:
    archive.parent.mkdir(parents=True, exist_ok=True)
    with archive.open("wb") as raw:
        with gzip.GzipFile(fileobj=raw, mode="wb", filename="", mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                for relative in files:
                    source = package / relative
                    info = tarfile.TarInfo(relative)
                    info.size = source.stat().st_size
                    info.mode = 0o644
                    info.mtime = 0
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    with source.open("rb") as contents:
                        tar.addfile(info, contents)


def verify_archive(archive: Path, files: list[dict[str, object]]) -> None:
    expected = {str(entry["path"]): entry for entry in files}
    with tarfile.open(archive, mode="r:gz") as tar:
        members = [member for member in tar.getmembers() if member.isfile()]
        actual_names = {member.name for member in members}
        if actual_names != set(expected):
            raise SystemExit("SDK archive inventory does not match its manifest")
        for member in members:
            data = tar.extractfile(member).read()  # type: ignore[union-attr]
            entry = expected[member.name]
            if len(data) != int(entry["size"]) or hashlib.sha256(data).hexdigest() != entry["sha256"]:
                raise SystemExit(f"SDK archive content does not match its manifest: {member.name}")


def package(output: Path, version: str, allow_dirty: bool, skip_build: Path | None) -> tuple[Path, Path]:
    commit, dirty = git_metadata(allow_dirty)
    output.mkdir(parents=True, exist_ok=True)
    archive = output / ARCHIVE_NAME
    manifest_path = output / MANIFEST_NAME
    if archive.exists():
        archive.unlink()
    if manifest_path.exists():
        manifest_path.unlink()

    with tempfile.TemporaryDirectory(prefix="lodestone-web-sdk-") as temporary:
        stage = Path(temporary) / "dist"
        if skip_build is None:
            run_trunk(stage)
        else:
            stage = skip_build.resolve()
        package_dir = Path(temporary) / "package"
        package_dir.mkdir()
        paths, entrypoint = collect_package(stage, package_dir)
        entries = [
            {"path": path, "size": (package_dir / path).stat().st_size, "sha256": sha256(package_dir / path)}
            for path in paths
        ]
        write_archive(package_dir, archive, paths)
        verify_archive(archive, entries)

    manifest = {
        "schema": "lodestone-web-sdk",
        "schema_version": 1,
        "version": version,
        "commit": commit,
        "dirty_checkout": dirty,
        "archive": {
            "path": archive.name,
            "size": archive.stat().st_size,
            "sha256": sha256(archive),
            "format": "tar.gz",
        },
        "entrypoint": entrypoint,
        "files": entries,
    }
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"SDK archive: {archive}")
    print(f"SDK manifest: {manifest_path}")
    print(f"SDK files: {len(entries)}; archive bytes: {archive.stat().st_size}")
    return archive, manifest_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=Path(os.environ.get("LODESTONE_WEB_SDK_DIR", DEFAULT_OUTPUT)))
    parser.add_argument("--version", default=os.environ.get("LODESTONE_WEB_SDK_VERSION", "26.2"))
    parser.add_argument("--allow-dirty", action="store_true", help="permit packaging from an uncommitted checkout")
    parser.add_argument("--skip-build", type=Path, metavar="DIST", help=argparse.SUPPRESS)
    args = parser.parse_args()
    package(args.output_dir.resolve(), args.version, args.allow_dirty, args.skip_build)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
