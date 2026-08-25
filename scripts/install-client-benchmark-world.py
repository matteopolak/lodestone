#!/usr/bin/env python3
"""Install the official Hermitcraft S10 Java world as a local benchmark oracle."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import stat
import tempfile
import urllib.request
import zipfile


ROOT = pathlib.Path(__file__).resolve().parents[1]
CACHE_ROOT = ROOT / ".cache" / "mc"
DOWNLOAD_URL = "https://r2.hermitcraft.com/hermitcraft10.zip"
EXPECTED_SHA256 = "f05bee362a8a93757ae984acc51b24a43da1f5456ee044d6171b3c440c922ffb"
ARCHIVE = CACHE_ROOT / "downloads" / "hermitcraft10.zip"
DESTINATION = CACHE_ROOT / "megaworld"
SERVER_JAR = CACHE_ROOT / "creative" / "server.jar"
MARKER = ".lodestone-benchmark-world.json"


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_archive_hash(path: pathlib.Path, expected: str) -> str:
    actual = sha256_file(path)
    if actual != expected:
        raise ValueError(
            f"archive SHA-256 mismatch: expected {expected}, got {actual}"
        )
    return actual


def download_archive(url: str, destination: pathlib.Path) -> None:
    """Stream one archive to a sibling temporary file, then publish it."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(f".{destination.name}.part-{os.getpid()}")
    request = urllib.request.Request(url, headers={"User-Agent": "Lodestone benchmark installer"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response, temporary.open("wb") as output:
            shutil.copyfileobj(response, output, length=1024 * 1024)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


def safe_extract(archive: pathlib.Path, destination: pathlib.Path) -> None:
    """Extract a ZIP only after rejecting traversal and symbolic-link entries."""
    destination.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as handle:
        for member in handle.infolist():
            normalized = member.filename.replace("\\", "/")
            parts = pathlib.PurePosixPath(normalized).parts
            file_type = stat.S_IFMT(member.external_attr >> 16)
            if (
                not normalized
                or normalized.startswith("/")
                or ".." in parts
                or file_type == stat.S_IFLNK
            ):
                raise ValueError(f"unsafe ZIP member: {member.filename!r}")
        handle.extractall(destination)


def find_world_root(extracted: pathlib.Path) -> pathlib.Path:
    """Return the archive's one directory containing level and region data."""
    candidates = []
    for level_dat in extracted.rglob("level.dat"):
        root = level_dat.parent
        region = root / "region"
        if region.is_dir() and any(region.glob("*.mca")):
            candidates.append(root)
    if len(candidates) != 1:
        raise ValueError(
            f"archive must contain exactly one Java world root, found {len(candidates)}"
        )
    return candidates[0]


def _server_properties() -> str:
    return "\n".join(
        [
            "server-port=25590",
            "enable-rcon=true",
            "rcon.port=25591",
            "rcon.password=lodestone",
            "online-mode=false",
            "enforce-secure-profile=false",
            "level-name=world",
            "gamemode=creative",
            "difficulty=peaceful",
            "allow-flight=true",
            "view-distance=25",
            "simulation-distance=10",
            "pause-when-empty-seconds=0",
            "motd=Lodestone large-world benchmark",
        ]
    ) + "\n"


def install_world_root(
    source: pathlib.Path,
    destination: pathlib.Path,
    server_jar: pathlib.Path,
    archive_sha256: str,
) -> bool:
    """Atomically install a validated world; return false for an identical cache."""
    marker = destination / MARKER
    if marker.is_file():
        metadata = json.loads(marker.read_text(encoding="utf-8"))
        if metadata.get("archive_sha256") == archive_sha256:
            return False
        raise FileExistsError(
            f"{destination} contains a different benchmark world; move it aside first"
        )
    if destination.exists():
        raise FileExistsError(
            f"{destination} exists without a complete-install marker; move it aside first"
        )
    if not server_jar.is_file():
        raise FileNotFoundError(f"Java 26.2 server jar not found: {server_jar}")

    destination.parent.mkdir(parents=True, exist_ok=True)
    staging = pathlib.Path(
        tempfile.mkdtemp(prefix=f".{destination.name}.installing-", dir=destination.parent)
    )
    try:
        shutil.copytree(source, staging / "world")
        shutil.copy2(server_jar, staging / "server.jar")
        (staging / "eula.txt").write_text("eula=true\n", encoding="utf-8")
        (staging / "server.properties").write_text(
            _server_properties(), encoding="utf-8"
        )
        (staging / MARKER).write_text(
            json.dumps(
                {
                    "archive_sha256": archive_sha256,
                    "source_url": DOWNLOAD_URL,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        staging.replace(destination)
    finally:
        if staging.exists():
            shutil.rmtree(staging)
    return True


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=pathlib.Path, help="use an existing ZIP")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    archive = args.archive.resolve() if args.archive else ARCHIVE
    if not archive.is_file():
        if args.archive:
            raise FileNotFoundError(archive)
        print(f"downloading {DOWNLOAD_URL} to {archive}", flush=True)
        download_archive(DOWNLOAD_URL, archive)
    archive_sha256 = verify_archive_hash(archive, EXPECTED_SHA256)
    print(f"archive sha256: {archive_sha256}", flush=True)

    with tempfile.TemporaryDirectory(prefix="megaworld-extract-", dir=CACHE_ROOT) as temp_name:
        extracted = pathlib.Path(temp_name)
        safe_extract(archive, extracted)
        source = find_world_root(extracted)
        installed = install_world_root(source, DESTINATION, SERVER_JAR, archive_sha256)
    print(
        f"{'installed' if installed else 'already installed'} benchmark world at {DESTINATION}",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, zipfile.BadZipFile, json.JSONDecodeError) as exc:
        raise SystemExit(f"benchmark world installation failed: {exc}") from exc
