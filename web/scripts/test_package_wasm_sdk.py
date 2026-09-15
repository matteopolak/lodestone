#!/usr/bin/env python3
"""Check the SDK inventory without compiling the browser bundle."""

from __future__ import annotations

import importlib.util
from pathlib import Path
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


def make_stage(packager, root: Path) -> Path:
    stage = root / "dist"
    stage.mkdir()
    files = {
        "lodestone-web-entry.js": b"export {}\n",
        "lodestone-web-entry_bg.wasm": b"wasm",
        "lodestone-web-page.js": b"export {}\n",
        "lodestone-web-page_bg.wasm": b"wasm",
    }
    for relative in packager.REQUIRED_FILES:
        files.setdefault(relative, b"asset")
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
        files, _ = packager.collect_package(stage, package)
        assert all(face in files for face in packager.PANORAMA_FILES)

        (stage / "panorama_5.png").unlink()
        missing_package = root / "missing-package"
        missing_package.mkdir()
        try:
            packager.collect_package(stage, missing_package)
        except SystemExit as error:
            assert "panorama_5.png" in str(error)
        else:
            raise AssertionError("SDK packaging accepted a missing panorama face")
    print("wasm SDK packaging checks: PASS (six faces included; missing-face control rejected)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
