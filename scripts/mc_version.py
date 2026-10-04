"""The current Minecraft reference version for scripts: `LODESTONE_MC_VERSION`,
else the repo-root `mc-version` file. Mirrors `lodestone_mc_cache` so the Rust
readers, the justfile and the scripts all name one source of truth. See
docs/mc-version-bump.md."""
import os
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def current_version() -> str:
    override = os.environ.get("LODESTONE_MC_VERSION", "").strip()
    return override or (ROOT / "mc-version").read_text().strip()


def cache_root() -> Path:
    """`.cache/mc/<current version>` in this repo."""
    return ROOT / ".cache" / "mc" / current_version()
