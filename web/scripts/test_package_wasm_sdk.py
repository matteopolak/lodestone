#!/usr/bin/env python3
"""Check the SDK inventory without compiling the browser bundle."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import re
import tempfile


ROOT = Path(__file__).resolve().parents[2]
PACKAGE_SCRIPT = ROOT / "web" / "scripts" / "package_wasm_sdk.py"


def load_packager():
    spec = importlib.util.spec_from_file_location("package_wasm_sdk", PACKAGE_SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {PACKAGE_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def make_stage(packager, root: Path, marker: bytes = b"first") -> Path:
    stage = root / "dist"
    stage.mkdir(parents=True)
    release_name = marker.decode("ascii")
    files = {
        "lodestone-web-entry.js": b"export {}\n",
        "lodestone-web-entry_bg.wasm": b"wasm",
        f"lodestone-web-{release_name}.js": b"export {}\n" + marker,
        f"lodestone-web-{release_name}_bg.wasm": b"wasm" + marker,
    }
    for relative in packager.REQUIRED_FILES:
        files.setdefault(relative, b"asset")
    files["lodestone-render-worker.js"] = (ROOT / "web" / "render_worker.js").read_bytes()
    for relative, contents in files.items():
        path = stage / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(contents)
    snippets = stage / "snippets" / "workerHelpers.no-bundler.js"
    snippets.parent.mkdir(parents=True)
    snippets.write_text("export {};\n", encoding="utf-8")
    return stage


def main() -> int:
    packager = load_packager()
    with tempfile.TemporaryDirectory(prefix="lodestone-wasm-sdk-test-") as directory:
        root = Path(directory)
        stage = make_stage(packager, root)
        package = root / "package"
        package.mkdir()
        files, entrypoint, worker_entrypoint = packager.collect_package(stage, package)
        assert all(face in files for face in packager.PANORAMA_FILES)
        assert worker_entrypoint in files
        assert "lodestone-render-worker.js" not in files
        assert "lodestone-web-entry.js" not in files
        assert "lodestone-web-entry_bg.wasm" not in files
        worker = (package / worker_entrypoint).read_text(encoding="utf-8")
        assert "assetProvider: packageAsset" in worker
        assert 'clientJar: "client.jar"' in worker
        assert 'blocksJson: "blocks.json"' in worker
        assert "PACKAGE_ASSET_PATHS[name] ?? name" in worker
        assert "fetch(new URL(path, self.location.href))" in worker
        mapping = dict(re.findall(r'^  ([A-Za-z][A-Za-z0-9]*): "([^"]+)",$', worker, re.MULTILINE))
        for logical_name, packaged_path in {
            "clientJar": "client.jar",
            "blocksJson": "blocks.json",
            **{face: face for face in packager.PANORAMA_FILES},
        }.items():
            resolved = mapping.get(logical_name, logical_name)
            assert resolved == packaged_path
            assert packaged_path in files
        assert "cache: \"no-store\"" in worker
        assert "manifest.entrypoint" in worker
        assert "SDK worker mismatch" in worker

        second_stage = make_stage(packager, root / "second", marker=b"second")
        second_package = root / "second-package"
        second_package.mkdir()
        _, second_entrypoint, second_worker = packager.collect_package(second_stage, second_package)
        assert second_entrypoint != entrypoint
        assert second_worker != worker_entrypoint

        (stage / "panorama_5.png").unlink()
        missing_package = root / "missing-package"
        missing_package.mkdir()
        try:
            packager.collect_package(stage, missing_package)
        except SystemExit as error:
            assert "panorama_5.png" in str(error)
        else:
            raise AssertionError("SDK packaging accepted a missing panorama face")
    print("wasm SDK packaging checks: PASS (versioned worker; asset mapping; release swap)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
