#!/usr/bin/env python3
"""Turn a `samply` capture + its `.syms.json` sidecar into a symbol-keyed,
`threadCPUDelta`-weighted cost table (issue #83).

This is the tool that turned a raw `samply` capture into "93.4% of
main-thread CPU inside `RenderState::render_inner`, of which
`Queue::write_buffer` was 52.9%" (`docs/section-camera-uniform.md`, issue
#75) -- packaged as a script instead of tribal knowledge in a closed issue.
See `docs/roadmap/benchmarks.md`'s "Profiling workflow" section for the full
recipe end to end (build flags, the `samply record` invocation, and how to
read this script's output).

# Usage

    samply record --save-only --unstable-presymbolicate -o profile.json.gz -- <binary> <args>
    python3 scripts/profile-cost-table.py profile.json.gz

Or, with an explicit sidecar path (auto-derived by default -- see
`--help`):

    python3 scripts/profile-cost-table.py profile.json.gz --syms profile.syms.json

# Why `threadCPUDelta`, not sample count

Sample-count weighting reads *blocked* time as work -- exactly backwards for
a client that spends real time waiting on `acquire()` (see
`docs/frame-pacing.md`'s occluded-`CAMetalLayer` finding, a different
investigation, same trap). This script uses `samples.threadCPUDelta` when
the capture has it, and only falls back to sample counts -- loudly, with a
warning banner -- when it does not (some platforms/captures do not record
CPU-delta at all). Falling back silently would reintroduce the exact
mistake issue #83 exists to stop repeating.

# The join, briefly (see `docs/roadmap/benchmarks.md` for the full account)

samply's saved profile keeps the per-thread `stackTable`/`frameTable`/
`funcTable`/`resourceTable` under a *shared* `profile["shared"]` object
(`fxprof-processed-profile`'s `RawProfileSharedData`), with `funcTable.name`
either a real symbol string or, for an address samply could not resolve at
record time, the literal hex string of that address's library-relative
offset (RVA) -- e.g. `"0x1a2b3c"` (`ProfileStringTable::
index_for_hex_address_string`, always lowercase, always `0x`-prefixed, no
padding). `funcTable.resource` indexes `resourceTable`, whose `lib` column
indexes the top-level `profile["libs"]` array, whose `debugName` is the key
this script joins against the sidecar's own `data[].debug_name`.

The sidecar (`<profile>.syms.json`, written by `samply`'s own precog/
presymbolicate machinery -- see `samply/src/shared/symbol_precog.rs`) is
`{"data": [...one entry per library...], "string_table": [...shared
strings...]}`. Each library entry is `{debug_name, debug_id, code_id,
symbol_table: [{rva, size, symbol, frames}, ...], known_addresses: [[rva,
symbol_table_index], ...]}` (`known_addresses` sorted by `rva`, one entry
per address actually seen in a captured stack -- *not* one entry per
function, since a function's whole address range shares one `symbol_table`
entry). `symbol`/`frames[].function` are indices into the shared
`string_table`. Resolving a raw `0x...` name is therefore: parse the hex
int, binary-search `known_addresses` for an *exact* match (not a
range/enclosing search -- samply itself does the same exact-match lookup,
see `PrecogLibraySymbolMap::lookup_sync`), and read `symbol_table[index]
.symbol` through `string_table`.

# What this reports

Two tables, both restricted to one thread (`--thread`, default: the
`isMainThread` thread, falling back to whichever thread has the most
samples):

- **Self time**: weight credited only to a sample's leaf (innermost) frame.
- **Inclusive time**: weight credited to every *distinct* function in a
  sample's stack (a function recursing does not get credited twice for the
  same sample), i.e. "how much of the thread's time was spent inside this
  function or something it called."

Neither is asserted against anything -- this is analysis tooling for a human
reading a profile, not a test.
"""

from __future__ import annotations

import argparse
import bisect
import gzip
import json
import re
import sys
from pathlib import Path

HEX_ADDR_RE = re.compile(r"^0x[0-9a-f]+$")


def load_json_maybe_gz(path: Path) -> dict:
    raw = path.read_bytes()
    if raw[:2] == b"\x1f\x8b":  # gzip magic
        raw = gzip.decompress(raw)
    return json.loads(raw)


def derive_syms_path(profile_path: Path) -> Path:
    """Mirrors samply's own `profile_path.with_extension("syms.json")`
    (`samply/src/main.rs`) for the common `*.json`/`*.json.gz` shapes, since
    Rust's `Path::with_extension` replaces only the *last* extension
    component -- for `profile.json.gz` that is `profile.json.syms.json`, not
    the perhaps-more-intuitive `profile.syms.json`. Both candidates are
    tried, in the order most captures actually produce."""
    name = profile_path.name
    candidates = []
    if name.endswith(".json.gz"):
        stem = name[: -len(".json.gz")]
        candidates.append(profile_path.with_name(stem + ".json.syms.json"))
        candidates.append(profile_path.with_name(stem + ".syms.json"))
    elif name.endswith(".json"):
        stem = name[: -len(".json")]
        candidates.append(profile_path.with_name(stem + ".syms.json"))
    else:
        candidates.append(profile_path.with_suffix(".syms.json"))
    for c in candidates:
        if c.exists():
            return c
    # None exist -- return the first candidate so the caller's error message
    # names a real, specific path rather than "somewhere near the profile".
    return candidates[0]


class SymsSidecar:
    """Parsed `<profile>.syms.json`, indexed for `resolve(debug_name, rva)`."""

    def __init__(self, raw: dict):
        self.string_table: list[str] = raw["string_table"]
        self.by_debug_name: dict[str, dict] = {}
        for lib in raw["data"]:
            known = lib["known_addresses"]  # [[rva, symbol_table_index], ...], sorted by rva
            self.by_debug_name[lib["debug_name"]] = {
                "symbol_table": lib["symbol_table"],
                "known_addresses": known,
                "known_rvas": [ka[0] for ka in known],  # parallel array for bisect
            }

    def resolve(self, debug_name: str | None, rva: int) -> str | None:
        if debug_name is None:
            return None
        lib = self.by_debug_name.get(debug_name)
        if lib is None:
            return None
        rvas = lib["known_rvas"]
        i = bisect.bisect_left(rvas, rva)
        if i >= len(rvas) or rvas[i] != rva:
            return None  # exact-match only, matching samply's own lookup_sync
        sym_index = lib["known_addresses"][i][1]
        symbol_str_index = lib["symbol_table"][sym_index]["symbol"]
        return self.string_table[symbol_str_index]


class Profile:
    """Parsed `profile.json`, with per-function name resolution (joining
    the sidecar for any still-raw `0x...` names) done once up front."""

    def __init__(self, raw: dict, syms: SymsSidecar | None):
        shared = raw["shared"]
        self.string_array: list[str] = shared["stringArray"]
        self.func_table = shared["funcTable"]
        self.frame_table = shared["frameTable"]
        self.stack_table = shared["stackTable"]
        self.resource_table = shared["resourceTable"]
        self.libs: list[dict] = raw["libs"]
        self.threads: list[dict] = raw["threads"]

        self.resolved_unsymbolicated = 0
        self.unresolved_unsymbolicated = 0
        self.func_name_cache: dict[int, str] = {}
        self._resolve_func_names(syms)

    def _lib_debug_name_for_resource(self, resource_index: int) -> str | None:
        if resource_index is None or resource_index < 0:
            return None
        lib_index = self.resource_table["lib"][resource_index]
        if lib_index is None:
            return None
        return self.libs[lib_index]["debugName"]

    def _resolve_func_names(self, syms: SymsSidecar | None) -> None:
        names = self.func_table["name"]
        resources = self.func_table["resource"]
        for func_index, name_str_index in enumerate(names):
            raw_name = self.string_array[name_str_index]
            if HEX_ADDR_RE.match(raw_name) and syms is not None:
                debug_name = self._lib_debug_name_for_resource(resources[func_index])
                rva = int(raw_name, 16)
                resolved = syms.resolve(debug_name, rva)
                if resolved is not None:
                    self.func_name_cache[func_index] = resolved
                    self.resolved_unsymbolicated += 1
                    continue
                self.unresolved_unsymbolicated += 1
            self.func_name_cache[func_index] = raw_name

    def func_name(self, func_index: int) -> str:
        return self.func_name_cache.get(func_index, f"<func {func_index}>")

    def pick_thread(self, name_substring: str | None) -> dict:
        if name_substring is not None:
            matches = [t for t in self.threads if name_substring in t.get("name", "")]
            if not matches:
                available = ", ".join(sorted({t.get("name", "?") for t in self.threads}))
                raise SystemExit(
                    f"no thread name contains {name_substring!r} -- available: {available}"
                )
            return max(matches, key=lambda t: t["samples"].get("length", 0))
        main = [t for t in self.threads if t.get("isMainThread")]
        if main:
            return main[0]
        return max(self.threads, key=lambda t: t["samples"].get("length", 0))


def stack_chain(stack_table: dict, stack_index: int) -> list[int]:
    """Frame indices from leaf to root for one sample's stack, walking
    `prefixOffset` (parent = i - prefixOffset[i]; 0 means root)."""
    frames = stack_table["frame"]
    prefix_offsets = stack_table["prefixOffset"]
    chain = []
    i = stack_index
    while True:
        chain.append(frames[i])
        offset = prefix_offsets[i]
        if offset == 0:
            break
        i -= offset
    return chain


def compute_cost_tables(profile: Profile, thread: dict) -> tuple[dict, dict, float, str]:
    """Returns `(self_time, inclusive_time, total_weight, weight_kind)`,
    where `weight_kind` is `"threadCPUDelta"` or `"sample count (fallback)"`.
    """
    samples = thread["samples"]
    stacks = samples["stack"]
    cpu_delta = samples.get("threadCPUDelta")
    weight_array = samples.get("weight")

    if cpu_delta is not None and any(d is not None for d in cpu_delta):
        weight_kind = "threadCPUDelta"

        def weight_of(i: int) -> float:
            v = cpu_delta[i]
            return 0.0 if v is None else float(v)
    else:
        weight_kind = "sample count (fallback)"
        print(
            "WARNING: this capture has no threadCPUDelta data -- falling back to "
            "sample-count weighting, which reads *blocked* time (e.g. acquire() "
            "stalls) as work. Re-record if this profile is meant to attribute CPU "
            "cost.",
            file=sys.stderr,
        )

        def weight_of(i: int) -> float:
            if weight_array is not None and weight_array[i] is not None:
                return float(weight_array[i])
            return 1.0

    frame_func = profile.frame_table["func"]
    self_time: dict[str, float] = {}
    inclusive_time: dict[str, float] = {}
    total = 0.0

    for i, stack_index in enumerate(stacks):
        if stack_index is None:
            continue
        w = weight_of(i)
        total += w
        chain = stack_chain(profile.stack_table, stack_index)
        leaf_func = frame_func[chain[0]]
        leaf_name = profile.func_name(leaf_func)
        self_time[leaf_name] = self_time.get(leaf_name, 0.0) + w

        seen = set()
        for frame_index in chain:
            func_index = frame_func[frame_index]
            if func_index in seen:
                continue  # recursion: credit a function once per sample
            seen.add(func_index)
            name = profile.func_name(func_index)
            inclusive_time[name] = inclusive_time.get(name, 0.0) + w

    return self_time, inclusive_time, total, weight_kind


def render_table(title: str, costs: dict, total: float, top: int) -> str:
    lines = [title]
    if total <= 0:
        lines.append("  (no weight recorded)")
        return "\n".join(lines)
    ranked = sorted(costs.items(), key=lambda kv: kv[1], reverse=True)
    for name, value in ranked[:top]:
        pct = 100.0 * value / total
        lines.append(f"  {pct:6.2f}%  {value:14.2f}  {name}")
    if len(ranked) > top:
        lines.append(f"  ... and {len(ranked) - top} more")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Join a samply profile with its .syms.json sidecar into a "
            "symbol-keyed, threadCPUDelta-weighted cost table."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Example:\n"
            "  samply record --save-only --unstable-presymbolicate \\\n"
            "    -o profile.json.gz -- ./target/release/lodestone\n"
            "  python3 scripts/profile-cost-table.py profile.json.gz\n"
        ),
    )
    parser.add_argument("profile", type=Path, help="path to the samply profile (.json or .json.gz)")
    parser.add_argument(
        "--syms",
        type=Path,
        default=None,
        help="path to the .syms.json sidecar (default: derived from <profile>, matching "
        "samply's own naming)",
    )
    parser.add_argument(
        "--thread",
        default=None,
        help="substring of the thread name to report on (default: the isMainThread "
        "thread, or the thread with the most samples if none is marked)",
    )
    parser.add_argument(
        "--top",
        type=int,
        default=20,
        help="how many functions to print per table (default: 20)",
    )
    args = parser.parse_args()

    if not args.profile.exists():
        parser.error(f"profile not found: {args.profile}")

    syms_path = args.syms or derive_syms_path(args.profile)
    syms = None
    if syms_path.exists():
        syms = SymsSidecar(load_json_maybe_gz(syms_path))
    else:
        print(
            f"WARNING: no sidecar found at {syms_path} -- unsymbolicated ('0x...') "
            "frames will be reported as raw addresses instead of function names. "
            "Re-record with --unstable-presymbolicate, or pass --syms explicitly.",
            file=sys.stderr,
        )

    raw_profile = load_json_maybe_gz(args.profile)
    profile = Profile(raw_profile, syms)
    thread = profile.pick_thread(args.thread)

    self_time, inclusive_time, total, weight_kind = compute_cost_tables(profile, thread)

    print(f"thread: {thread.get('name', '?')!r}  weight: {weight_kind}  total: {total:.2f}")
    if syms is not None:
        print(
            f"symbolicated {profile.resolved_unsymbolicated} raw address(es) via sidecar, "
            f"{profile.unresolved_unsymbolicated} unresolved"
        )
    print()
    print(render_table("Inclusive (function or something it called):", inclusive_time, total, args.top))
    print()
    print(render_table("Self (leaf only):", self_time, total, args.top))
    return 0


if __name__ == "__main__":
    sys.exit(main())
