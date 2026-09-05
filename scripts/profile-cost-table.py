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

**Verified against samply 0.13.1** (a real capture, not only a fixture --
see `scripts/test-profile-cost-table.py`). If you upgrade samply, run that
test: this script parses samply's saved-profile format, and that format has
already moved once underneath it (issue #83's original version read only the
newer hoisted layout and died with `KeyError: 'shared'` on every 0.13.1
capture).

# Usage

    samply record --save-only --unstable-presymbolicate -o profile.json.gz -- <binary> <args>
    python3 scripts/profile-cost-table.py profile.json.gz

Or, with an explicit sidecar path (auto-derived by default -- see
`--help`):

    python3 scripts/profile-cost-table.py profile.json.gz --syms profile.json.syms.json

# Why `threadCPUDelta`, not sample count

Sample-count weighting reads *blocked* time as work -- exactly backwards for
a client that spends real time waiting on `acquire()` (see
`docs/frame-pacing.md`'s occluded-`CAMetalLayer` finding, a different
investigation, same trap). This script uses `samples.threadCPUDelta` when
the capture has it, and only falls back to sample counts -- loudly, with a
warning banner -- when it does not (some platforms/captures do not record
CPU-delta at all). Falling back silently would reintroduce the exact
mistake issue #83 exists to stop repeating.

# Two profile layouts, dispatched on `preprocessedProfileVersion`

samply's saved profile carries the `stackTable`/`frameTable`/`funcTable`/
`resourceTable`/`stringArray` tables in one of two places, and which one is
a property of the *format version*, not of the capture:

| layout | `preprocessedProfileVersion` | tables live in |
|---|---|---|
| per-thread | absent, or <= 55 | `profile["threads"][i]` -- one set **per thread** |
| hoisted | >= 56 | `profile["shared"]` -- one set for the whole profile |

**samply 0.13.1 emits the per-thread layout and does not emit the
`preprocessedProfileVersion` key at all** (measured: `--save-only`
`--unstable-presymbolicate` writes `meta.version: 24`, no
`preprocessedProfileVersion`, tables under each `threads[i]`). An absent key
therefore means "per-thread", not "unknown".

Dispatch is on the version rather than on `"shared" in profile` deliberately:
a presence check silently picks a branch when a future format moves things
again, and a wrongly-picked branch here does not crash -- it reports a
plausible-looking table for the wrong thread's function indices. Anything
newer than `MAX_KNOWN_PROFILE_VERSION` is a hard error naming the version,
and a layout that contradicts its own version (a `shared` object in a
per-thread-versioned profile, or vice versa) is a hard error too.

**In the per-thread layout the tables are thread-local and function indices
are NOT comparable across threads.** All resolution and every table lookup
here is therefore scoped to one thread's tables.

# The stack chain: `prefix` vs `prefixOffset`

Two encodings, distinguished by which key is present (they are
semantically distinct, so this cannot silently pick wrong):

- `stackTable.prefix[i]` -- the parent's **index**, `null` at the root.
  This is what samply 0.13.1 writes.
- `stackTable.prefixOffset[i]` -- a **delta**: parent = `i - offset`, `0` at
  the root. The newer, delta-encoded form.

Reading one as the other is the silent-wrong-answer case this names
explicitly: on a real 0.13.1 capture `prefix` begins `[null, 0, 1, 2, ...]`,
so reading it as an offset makes stack 1 its own parent (an infinite loop)
and stack 8 (`prefix` 6) report a grandparent as its parent.

# The symbol join: keyed on `(library, address)`, never address alone

`funcTable.name` is either a real symbol string or, for an address samply
could not resolve at record time, the literal hex string of that address's
library-relative offset (RVA) -- e.g. `"0x1a2b3c"` (`ProfileStringTable::
index_for_hex_address_string`, always lowercase, always `0x`-prefixed, no
padding). `funcTable.resource` indexes `resourceTable`, whose `lib` column
indexes the top-level `profile["libs"]` array, whose `debugName` is the key
this script joins against the sidecar's own `data[].debug_name`.

The sidecar (`<profile>.json.syms.json`, written by `samply`'s own precog/
presymbolicate machinery -- see `samply/src/shared/symbol_precog.rs`) is
`{"data": [...one entry per library...], "string_table": [...shared
strings...]}`. Each library entry is `{debug_name, debug_id, code_id,
symbol_table: [{rva, size, symbol, frames}, ...], known_addresses: [[rva,
symbol_table_index], ...]}` (one entry per address actually seen in a
captured stack -- *not* one entry per function, since a function's whole
address range shares one `symbol_table` entry). `symbol`/`frames[].function`
are indices into the shared `string_table`.

**`known_addresses` are library-relative, so they collide across
libraries** -- every library's `.text` starts at a small RVA, and a
2-library capture routinely has two different symbols at the same offset.
The join key is therefore `(debug_name, rva)`. An address-only join is not
a crash, it is a *silent misattribution*: cost lands on a symbol from
whichever library happened to be indexed last.
`scripts/test-profile-cost-table.py` carries that as an executed control --
it builds a colliding fixture, joins it on address alone, and asserts the
attribution is wrong -- so the key stays load-bearing rather than
decorative.

Resolving a raw `0x...` name is: parse the hex int, look up
`(debug_name, rva)` for an *exact* match (not a range/enclosing search --
samply itself does the same exact-match lookup, see
`PrecogLibraySymbolMap::lookup_sync`), and read
`symbol_table[index].symbol` through `string_table`.

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
import gzip
import json
import math
import re
import sys
from pathlib import Path

HEX_ADDR_RE = re.compile(r"^0x[0-9a-f]+$")

LAYOUT_PER_THREAD = "per-thread"
LAYOUT_SHARED = "shared"

# Highest `preprocessedProfileVersion` whose layout this script has actually
# been reasoned about against. Anything above it is a hard error rather than
# a guess -- bump this only after checking what moved (and add a fixture to
# `scripts/test-profile-cost-table.py`).
MAX_KNOWN_PROFILE_VERSION = 60

# First version with the hoisted `profile["shared"]` tables.
FIRST_SHARED_LAYOUT_VERSION = 56


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
    the perhaps-more-intuitive `profile.syms.json`. Measured against samply
    0.13.1: `-o u20p.json.gz` writes `u20p.json.syms.json`. Both spellings
    are tried either way, in the order captures actually produce them, so a
    hand-renamed sidecar still resolves."""
    name = profile_path.name
    candidates = []
    if name.endswith(".json.gz"):
        stem = name[: -len(".json.gz")]
        candidates.append(profile_path.with_name(stem + ".json.syms.json"))
        candidates.append(profile_path.with_name(stem + ".syms.json"))
    elif name.endswith(".json"):
        stem = name[: -len(".json")]
        candidates.append(profile_path.with_name(stem + ".syms.json"))
        candidates.append(profile_path.with_name(stem + ".json.syms.json"))
    else:
        candidates.append(profile_path.with_suffix(".syms.json"))
        candidates.append(profile_path.with_name(name + ".syms.json"))
    for c in candidates:
        if c.exists():
            return c
    # None exist -- return the first candidate so the caller's error message
    # names a real, specific path rather than "somewhere near the profile".
    return candidates[0]


class SymsSidecar:
    """Parsed `<profile>.json.syms.json`, indexed for
    `resolve(debug_name, rva)`.

    The index is keyed on `(debug_name, rva)`. See the module docstring: RVAs
    are library-relative and collide across libraries, so an address-only
    index misattributes cost instead of failing."""

    def __init__(self, raw: dict):
        self.string_table: list[str] = raw["string_table"]
        # (debug_name, rva) -> index into that library's symbol_table.
        self.by_lib_and_rva: dict[tuple[str, int], int] = {}
        self.symbol_tables: dict[str, list[dict]] = {}
        self.libraries: list[str] = []
        for lib in raw["data"]:
            debug_name = lib["debug_name"]
            symbol_table = lib["symbol_table"]
            if debug_name in self.symbol_tables:
                # Two entries for one debugName (the same binary mapped
                # twice, say). Keep the first table and skip the duplicate
                # rather than silently replacing it -- overwriting would
                # invalidate every already-recorded symbol_table index.
                print(
                    f"WARNING: sidecar has more than one entry for debugName "
                    f"{debug_name!r}; using the first and ignoring the rest.",
                    file=sys.stderr,
                )
                continue
            self.symbol_tables[debug_name] = symbol_table
            self.libraries.append(debug_name)
            for rva, symbol_table_index in lib["known_addresses"]:
                self.by_lib_and_rva[(debug_name, rva)] = symbol_table_index

    def resolve(self, debug_name: str | None, rva: int) -> str | None:
        if debug_name is None:
            return None
        sym_index = self.by_lib_and_rva.get((debug_name, rva))
        if sym_index is None:
            return None  # exact-match only, matching samply's own lookup_sync
        symbol_table = self.symbol_tables.get(debug_name)
        if symbol_table is None or sym_index >= len(symbol_table):
            return None
        symbol_str_index = symbol_table[sym_index]["symbol"]
        return self.string_table[symbol_str_index]


def detect_layout(raw: dict) -> tuple[str, int | None]:
    """`(layout, version)` for a saved profile, dispatched on
    `preprocessedProfileVersion`.

    Raises `SystemExit` -- loudly, naming the version -- rather than guessing,
    both for a version above `MAX_KNOWN_PROFILE_VERSION` and for a profile
    whose actual shape contradicts its version."""
    version = raw.get("preprocessedProfileVersion")
    if version is not None and not isinstance(version, int):
        raise SystemExit(
            f"preprocessedProfileVersion is {version!r} ({type(version).__name__}), "
            "expected an integer -- this is not a shape this script knows how to read."
        )

    if version is None:
        # samply 0.13.1 and earlier do not emit the key. Per-thread tables.
        layout = LAYOUT_PER_THREAD
    elif version < FIRST_SHARED_LAYOUT_VERSION:
        layout = LAYOUT_PER_THREAD
    elif version <= MAX_KNOWN_PROFILE_VERSION:
        layout = LAYOUT_SHARED
    else:
        raise SystemExit(
            f"profile has preprocessedProfileVersion {version}, which is newer than the "
            f"newest version this script has been checked against "
            f"({MAX_KNOWN_PROFILE_VERSION}). The table layout may have moved again. "
            "Check samply's/fxprof-processed-profile's format, then raise "
            "MAX_KNOWN_PROFILE_VERSION in scripts/profile-cost-table.py and add a "
            "fixture to scripts/test-profile-cost-table.py."
        )

    # A version-derived layout that the file itself contradicts is the exact
    # failure a presence check would have papered over. Fail, don't guess.
    has_shared = isinstance(raw.get("shared"), dict)
    version_text = "absent" if version is None else str(version)
    if layout == LAYOUT_SHARED and not has_shared:
        raise SystemExit(
            f"preprocessedProfileVersion {version_text} implies hoisted tables under "
            'profile["shared"], but there is no "shared" object. Refusing to guess a '
            "layout."
        )
    if layout == LAYOUT_PER_THREAD and has_shared:
        raise SystemExit(
            f"preprocessedProfileVersion {version_text} implies per-thread tables, but "
            'the profile has a hoisted profile["shared"] object. Refusing to guess a '
            "layout."
        )
    return layout, version


class Tables:
    """One set of `stringArray`/`funcTable`/`frameTable`/`stackTable`/
    `resourceTable`, plus function names resolved through the sidecar.

    In the per-thread layout there is one of these *per thread* and its
    function indices mean nothing in another thread's tables."""

    def __init__(self, source: dict, libs: list[dict], syms: SymsSidecar | None, where: str):
        for key in ("stringArray", "funcTable", "frameTable", "stackTable", "resourceTable"):
            if key not in source:
                raise SystemExit(f"{where} has no {key!r} -- not a layout this script knows.")
        self.string_array: list[str] = source["stringArray"]
        self.func_table = source["funcTable"]
        self.frame_table = source["frameTable"]
        self.stack_table = source["stackTable"]
        self.resource_table = source["resourceTable"]
        self.libs = libs

        self.resolved_unsymbolicated = 0
        self.unresolved_unsymbolicated = 0
        self.func_name_cache: dict[int, str] = {}
        self._resolve_func_names(syms)

    def _lib_debug_name_for_resource(self, resource_index: int | None) -> str | None:
        if resource_index is None or resource_index < 0:
            return None
        lib_index = self.resource_table["lib"][resource_index]
        if lib_index is None or lib_index < 0:
            return None
        return self.libs[lib_index].get("debugName")

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

    def stack_chain(self, stack_index: int) -> list[int]:
        """Frame indices from leaf to root for one sample's stack.

        Accepts both encodings (see the module docstring): `prefix` holds the
        parent's index with `null` at the root, `prefixOffset` holds a delta
        with `0` at the root."""
        frames = self.stack_table["frame"]
        prefix = self.stack_table.get("prefix")
        prefix_offsets = self.stack_table.get("prefixOffset")
        if prefix is None and prefix_offsets is None:
            raise SystemExit(
                "stackTable has neither 'prefix' (parent index, null at root) nor "
                "'prefixOffset' (parent = i - offset, 0 at root) -- cannot walk stacks."
            )
        chain = []
        i = stack_index
        seen: set[int] = set()
        while True:
            if i in seen:  # malformed table; refuse to spin forever
                raise SystemExit(
                    f"stackTable cycles at index {i} -- refusing to loop. This is what "
                    "reading 'prefix' as 'prefixOffset' looks like."
                )
            seen.add(i)
            chain.append(frames[i])
            if prefix is not None:
                parent = prefix[i]
                if parent is None:
                    break
                i = parent
            else:
                offset = prefix_offsets[i]
                if offset == 0:
                    break
                i -= offset
        return chain


class Profile:
    """Parsed `profile.json`, with per-function name resolution (joining
    the sidecar for any still-raw `0x...` names) done once per table set."""

    def __init__(self, raw: dict, syms: SymsSidecar | None):
        self.layout, self.version = detect_layout(raw)
        self.libs: list[dict] = raw["libs"]
        self.threads: list[dict] = raw["threads"]
        self._syms = syms
        self._shared_tables: Tables | None = None
        self._thread_tables: dict[int, Tables] = {}
        if self.layout == LAYOUT_SHARED:
            self._shared_tables = Tables(raw["shared"], self.libs, syms, 'profile["shared"]')

    def tables_for(self, thread_index: int) -> Tables:
        if self._shared_tables is not None:
            return self._shared_tables
        cached = self._thread_tables.get(thread_index)
        if cached is None:
            thread = self.threads[thread_index]
            cached = Tables(
                thread,
                self.libs,
                self._syms,
                f'profile["threads"][{thread_index}]',
            )
            self._thread_tables[thread_index] = cached
        return cached

    def pick_thread(self, name_substring: str | None) -> int:
        """Index into `self.threads` -- an index, not the dict, because in the
        per-thread layout the caller needs it to fetch that thread's tables."""

        def sample_count(index: int) -> int:
            return self.threads[index]["samples"].get("length", 0) or 0

        indices = range(len(self.threads))
        if name_substring is not None:
            matches = [i for i in indices if name_substring in (self.threads[i].get("name") or "")]
            if not matches:
                available = ", ".join(sorted({t.get("name") or "?" for t in self.threads}))
                raise SystemExit(
                    f"no thread name contains {name_substring!r} -- available: {available}"
                )
            return max(matches, key=sample_count)
        main = [i for i in indices if self.threads[i].get("isMainThread")]
        if main:
            return main[0]
        if not self.threads:
            raise SystemExit("profile has no threads")
        return max(indices, key=sample_count)


def require_positive_cpu_time(thread: dict) -> None:
    """Reject a selected thread that cannot support CPU-cost attribution.

    The default report remains useful for inspecting captures from platforms
    that omit ``threadCPUDelta``. A performance claim is a narrower use: every
    sampled stack must have a finite, non-negative CPU delta and their total
    must be positive. Otherwise a table can look complete while describing
    only blocked time or no work at all.
    """
    samples = thread["samples"]
    stacks = samples["stack"]
    cpu_delta = samples.get("threadCPUDelta")
    if not isinstance(cpu_delta, list):
        raise SystemExit(
            "selected thread has no threadCPUDelta array; cannot require CPU-time "
            "attribution. Re-record with a Samply setup that records CPU deltas."
        )
    if len(cpu_delta) != len(stacks):
        raise SystemExit(
            "selected thread has threadCPUDelta entries for "
            f"{len(cpu_delta)} samples but {len(stacks)} stack entries; refusing "
            "a misaligned CPU-time table."
        )

    missing = 0
    invalid = 0
    total = 0.0
    for stack, delta in zip(stacks, cpu_delta):
        if stack is None:
            continue
        if delta is None:
            missing += 1
            continue
        if isinstance(delta, bool) or not isinstance(delta, (int, float)):
            invalid += 1
            continue
        value = float(delta)
        if not math.isfinite(value) or value < 0:
            invalid += 1
            continue
        total += value

    if missing or invalid:
        raise SystemExit(
            "selected thread has incomplete threadCPUDelta data for sampled stacks "
            f"({missing} missing, {invalid} invalid); refusing CPU-cost attribution."
        )
    if total <= 0:
        raise SystemExit(
            "selected thread has no positive threadCPUDelta across sampled stacks; "
            "the capture contains no observed CPU work to attribute."
        )


def compute_cost_tables(tables: Tables, thread: dict) -> tuple[dict, dict, float, str]:
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

    frame_func = tables.frame_table["func"]
    self_time: dict[str, float] = {}
    inclusive_time: dict[str, float] = {}
    total = 0.0

    for i, stack_index in enumerate(stacks):
        if stack_index is None:
            continue
        w = weight_of(i)
        total += w
        chain = tables.stack_chain(stack_index)
        leaf_func = frame_func[chain[0]]
        leaf_name = tables.func_name(leaf_func)
        self_time[leaf_name] = self_time.get(leaf_name, 0.0) + w

        seen = set()
        for frame_index in chain:
            func_index = frame_func[frame_index]
            if func_index in seen:
                continue  # recursion: credit a function once per sample
            seen.add(func_index)
            name = tables.func_name(func_index)
            inclusive_time[name] = inclusive_time.get(name, 0.0) + w

    return self_time, inclusive_time, total, weight_kind


def render_table(title: str, costs: dict, total: float, top: int) -> str:
    lines = [title]
    if total <= 0:
        lines.append("  (no weight recorded)")
        return "\n".join(lines)
    ranked = sorted(costs.items(), key=lambda kv: (-kv[1], kv[0]))
    for name, value in ranked[:top]:
        pct = 100.0 * value / total
        lines.append(f"  {pct:6.2f}%  {value:14.2f}  {name}")
    if len(ranked) > top:
        lines.append(f"  ... and {len(ranked) - top} more")
    return "\n".join(lines)


def build_report(
    profile: Profile, thread_name: str | None, top: int, require_cpu_time: bool = False
) -> str:
    """The whole stdout body, as a string, so a test can assert on it."""
    thread_index = profile.pick_thread(thread_name)
    thread = profile.threads[thread_index]
    if require_cpu_time:
        require_positive_cpu_time(thread)
    tables = profile.tables_for(thread_index)
    self_time, inclusive_time, total, weight_kind = compute_cost_tables(tables, thread)

    version_text = "absent" if profile.version is None else str(profile.version)
    out = [
        f"layout: {profile.layout} (preprocessedProfileVersion: {version_text})",
        f"thread: {thread.get('name', '?')!r}  weight: {weight_kind}  total: {total:.2f}",
    ]
    if profile._syms is not None:
        out.append(
            f"symbolicated {tables.resolved_unsymbolicated} raw address(es) via sidecar, "
            f"{tables.unresolved_unsymbolicated} unresolved"
        )
    out.append("")
    out.append(
        render_table("Inclusive (function or something it called):", inclusive_time, total, top)
    )
    out.append("")
    out.append(render_table("Self (leaf only):", self_time, total, top))
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
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
            "\n"
            "Verified against samply 0.13.1. After a samply upgrade, run\n"
            "  python3 scripts/test-profile-cost-table.py\n"
        ),
    )
    parser.add_argument("profile", type=Path, help="path to the samply profile (.json or .json.gz)")
    parser.add_argument(
        "--syms",
        type=Path,
        default=None,
        help="path to the .syms.json sidecar (default: derived from <profile>, matching "
        "samply's own naming -- <profile>.json.syms.json for a .json.gz capture)",
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
    parser.add_argument(
        "--require-cpu-time",
        action="store_true",
        help="fail unless every sampled stack has a finite non-negative threadCPUDelta "
        "and their total is positive; use when the report supports a CPU-cost claim",
    )
    args = parser.parse_args(argv)

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
    print(build_report(profile, args.thread, args.top, args.require_cpu_time))
    return 0


if __name__ == "__main__":
    sys.exit(main())
