#!/usr/bin/env python3
"""Gate for `scripts/profile-cost-table.py` (issue #83's profiling workflow).

Run it directly; it needs nothing but a stdlib `python3`:

    python3 scripts/test-profile-cost-table.py

# Why this exists

The script it tests had **no gate at all**, and rotted undetected straight
through a samply upgrade: it read only the hoisted `profile["shared"]`
layout, so every capture from samply 0.13.1 (per-thread tables, no
`preprocessedProfileVersion` key) died with `KeyError: 'shared'`. Two agents
hit that independently before anyone fixed it. A format this script does not
own will move again; this file is what makes that a red test instead of a
lost afternoon.

# What the fixtures are, and which of them is an outside oracle

`fixtures/profile-cost-table/`:

- **`samply-0.13.1-real.json.gz` + `.json.syms.json`** -- a **real samply
  0.13.1 capture**, subsampled to every 11th sample (311 of 3421) to keep it
  a few KB. The format is samply's, not ours, which is the whole point: it is
  the one fixture here whose shape no one on this side authored. Absolute
  totals are therefore ~1/11 of the real capture; the stack distribution and
  every structural field are untouched.

  Its subject is a 3-function probe (`u20_hot_gamma` calls `u20_hot_alpha(n)`
  and `u20_hot_beta(n/4)`), so the expected **ratio** of leaf cost originates
  outside this code entirely -- from the probe's own loop bounds. That is the
  outside-the-code-under-test expected value the evidence standard asks for,
  and it is what distinguishes "the join did not crash" from "the join
  attributed to the right symbols".

- **`collide-per-thread-v55.json`** / **`collide-shared-v56.json`** (+
  sidecars) -- hand-built, deliberately tiny, and deliberately *colliding*:
  RVA `0x1000` exists in **both** `liba` and `libb` with different symbols.
  These are synthetic and only prove what we chose to model, so they carry
  the assertions a real capture cannot: exact weights, both profile layouts,
  and the cross-library collision.

# The controls, and why each one is here

Every `control_*` below runs a *deliberately wrong* variant and **requires
the wrong answer**. A control that quietly agrees with the correct one is
reported as a failure of the control, not a pass -- otherwise the assertions
above it could be vacuous and nothing would say so.
"""

from __future__ import annotations

import copy
import gzip
import importlib.util
import json
import os
import re
import sys
import tempfile
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures" / "profile-cost-table"

# `PROFILE_COST_TABLE_PATH` points this suite at a *copy* of the script, which
# is how the fixes here were mutation-tested: break one thing in a scratch
# copy, run this file, and confirm it goes red. A suite that stays green
# against a deliberately broken script is not a gate, and this override is
# what makes that answerable in one command instead of by editing the real
# file in a shared checkout.
_script = Path(os.environ.get("PROFILE_COST_TABLE_PATH") or (HERE / "profile-cost-table.py"))
_spec = importlib.util.spec_from_file_location("pct", _script)
pct = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pct)


# --------------------------------------------------------------------------
# harness
# --------------------------------------------------------------------------

FAILURES: list[str] = []


def check(name: str, fn) -> None:
    try:
        fn()
    except Exception:
        FAILURES.append(f"{name}\n{traceback.format_exc()}")
        print(f"FAIL {name}")
    else:
        print(f"ok   {name}")


class ControlDidNotFail(AssertionError):
    """A control that was supposed to produce the wrong answer produced the
    right one -- so it proves nothing about the assertion it guards."""


def require_wrong(what: str, fn) -> None:
    """Run `fn`, which asserts the *correct* answer against deliberately
    broken input. It must raise `AssertionError`."""
    try:
        fn()
    except ControlDidNotFail:
        raise
    except AssertionError:
        return  # good: the broken input really does break the assertion
    raise ControlDidNotFail(
        f"control {what!r} did not fail -- the assertion it guards cannot distinguish "
        "correct from broken input, so it is vacuous."
    )


def load_fixture(name: str) -> dict:
    path = FIXTURES / name
    return pct.load_json_maybe_gz(path)


def sidecar_path_for(name: str) -> Path:
    """Deliberately routed through the script's own derivation, so the
    fixtures also exercise samply's real sidecar naming."""
    return pct.derive_syms_path(FIXTURES / name)


def sidecar_for(name: str) -> pct.SymsSidecar:
    return pct.SymsSidecar(pct.load_json_maybe_gz(sidecar_path_for(name)))


def report(profile_raw: dict, syms, thread: str | None = None, top: int = 20) -> str:
    return pct.build_report(pct.Profile(profile_raw, syms), thread, top)


def parse_section(text: str, heading: str) -> dict[str, float]:
    """`{symbol: weight}` from one rendered table."""
    out: dict[str, float] = {}
    lines = text.splitlines()
    try:
        start = next(i for i, ln in enumerate(lines) if ln.startswith(heading))
    except StopIteration:
        raise AssertionError(f"no {heading!r} section in:\n{text}")
    for ln in lines[start + 1 :]:
        m = re.match(r"^\s+([\d.]+)%\s+([\d.]+)\s+(.*)$", ln)
        if not m:
            break
        out[m.group(3)] = float(m.group(2))
    return out


SELF = "Self (leaf only):"
INCL = "Inclusive (function or something it called):"

REAL = "samply-0.13.1-real.json.gz"
V55 = "collide-per-thread-v55.json"
V56 = "collide-shared-v56.json"


# --------------------------------------------------------------------------
# 1. the real samply 0.13.1 capture
# --------------------------------------------------------------------------


def test_real_capture_parses_and_attributes() -> None:
    text = report(load_fixture(REAL), sidecar_for(REAL))
    assert "layout: per-thread (preprocessedProfileVersion: absent)" in text, text
    assert "weight: threadCPUDelta" in text, text
    # Every func name in this capture is an unresolved hex address at record
    # time, so the sidecar join is fully load-bearing here: 0 resolved would
    # mean the table below is addresses, not symbols.
    assert "symbolicated 23 raw address(es) via sidecar, 0 unresolved" in text, text

    self_time = parse_section(text, SELF)
    assert set(self_time) == {
        "u20probe::u20_hot_alpha",
        "u20probe::u20_hot_beta",
    }, f"unexpected leaves: {sorted(self_time)}"

    # The probe's `u20_hot_gamma` calls alpha(n) and beta(n/4). The expected
    # ratio therefore comes from the probe's source, not from this script.
    #   correct-attribution hypothesis: alpha/beta ~= 4
    #   swapped-attribution hypothesis: alpha/beta ~= 0.25
    # Requiring the measurement to land on one of the two is the point; a
    # bare "alpha > beta" would be satisfied by far too much.
    ratio = self_time["u20probe::u20_hot_alpha"] / self_time["u20probe::u20_hot_beta"]
    assert 2.5 < ratio < 6.5, f"alpha/beta leaf ratio {ratio:.3f} matches neither hypothesis"

    incl = parse_section(text, INCL)
    for expected in ("main", "u20probe::main", "u20probe::u20_hot_gamma"):
        assert expected in incl, f"{expected!r} missing from inclusive table: {sorted(incl)}"
    # gamma calls both leaves, so it must carry the whole thread.
    assert incl["u20probe::u20_hot_gamma"] == sum(self_time.values())


def control_real_capture_without_sidecar_is_raw_addresses() -> None:
    """Premise check for the assertion above: if the sidecar were not doing
    the work, the leaves would be hex addresses. Proves `23 resolved` is a
    real join and not a coincidence of a pre-symbolicated capture."""
    text = report(load_fixture(REAL), None)
    self_time = parse_section(text, SELF)
    assert self_time, text
    assert all(re.match(r"^0x[0-9a-f]+$", k) for k in self_time), (
        f"expected raw addresses with no sidecar, got {sorted(self_time)}"
    )
    require_wrong(
        "real capture with no sidecar still names symbols",
        lambda: _assert_symbols_present(self_time),
    )


def _assert_symbols_present(self_time: dict[str, float]) -> None:
    assert "u20probe::u20_hot_alpha" in self_time


def control_real_capture_corrupted_symbol_table() -> None:
    """Deliberately corrupted fixture: rename the hot symbol in the sidecar
    and require the attribution assertion to fail."""
    raw_syms = pct.load_json_maybe_gz(sidecar_path_for(REAL))
    corrupted = copy.deepcopy(raw_syms)
    st = corrupted["string_table"]
    for i, s in enumerate(st):
        if s == "u20probe::u20_hot_alpha":
            st[i] = "u20probe::NOT_THE_REAL_SYMBOL"
    assert "u20probe::NOT_THE_REAL_SYMBOL" in st, "fixture no longer contains the hot symbol"

    def assert_correct() -> None:
        text = report(load_fixture(REAL), pct.SymsSidecar(corrupted))
        self_time = parse_section(text, SELF)
        assert "u20probe::u20_hot_alpha" in self_time, f"leaves: {sorted(self_time)}"

    require_wrong("corrupted sidecar symbol string", assert_correct)


def control_real_capture_truncated_samples() -> None:
    """A capture whose samples are emptied must not still report a table."""
    broken = load_fixture(REAL)
    s = broken["threads"][0]["samples"]
    s["stack"] = []
    s["threadCPUDelta"] = []
    s["length"] = 0

    def assert_correct() -> None:
        text = report(broken, sidecar_for(REAL))
        assert parse_section(text, SELF), "expected a populated self-time table"

    require_wrong("emptied samples", assert_correct)


# --------------------------------------------------------------------------
# 2. the (library, address) join key
# --------------------------------------------------------------------------

# Hand-derived from the fixture, by hand, not by running the script:
#   sample 0 -> stack 1, leaf liba@0x1000, cpu 10
#   samples 1,2 -> stack 2, leaf libb@0x1000, cpu 20 + 70 = 90
#   stack chains: 1 = [liba, plain_main], 2 = [libb, liba, plain_main]
EXPECTED_SELF = {"liba::alpha_symbol": 10.0, "libb::beta_symbol": 90.0}
EXPECTED_INCL = {
    "fixture_plain_main": 100.0,
    "liba::alpha_symbol": 100.0,
    "libb::beta_symbol": 90.0,
}


def test_collision_fixture_per_thread_v55() -> None:
    text = report(load_fixture(V55), sidecar_for(V55))
    assert "layout: per-thread (preprocessedProfileVersion: 55)" in text, text
    assert parse_section(text, SELF) == EXPECTED_SELF, text
    assert parse_section(text, INCL) == EXPECTED_INCL, text


def test_collision_fixture_shared_v56() -> None:
    """The hoisted layout, with `prefixOffset` stacks, must produce the
    identical table -- the two layouts are two spellings of one profile."""
    text = report(load_fixture(V56), sidecar_for(V56))
    assert "layout: shared (preprocessedProfileVersion: 56)" in text, text
    assert parse_section(text, SELF) == EXPECTED_SELF, text
    assert parse_section(text, INCL) == EXPECTED_INCL, text


class AddressOnlySidecar(pct.SymsSidecar):
    """The pre-fix join: one index keyed on the RVA alone, ignoring which
    library it came from. Kept here as an executed control, so the
    `(library, address)` key stays load-bearing instead of decorative."""

    def __init__(self, raw: dict):
        self.string_table = raw["string_table"]
        self.by_rva: dict[int, tuple[str, int]] = {}
        self.symbol_tables = {}
        for lib in raw["data"]:
            self.symbol_tables[lib["debug_name"]] = lib["symbol_table"]
            for rva, idx in lib["known_addresses"]:
                self.by_rva[rva] = (lib["debug_name"], idx)  # last library wins

    def resolve(self, debug_name, rva):
        hit = self.by_rva.get(rva)
        if hit is None:
            return None
        owner, idx = hit
        return self.string_table[self.symbol_tables[owner][idx]["symbol"]]


def control_address_only_join_misattributes() -> None:
    """The evidence that the address-only key was *wrong*, not merely
    old-fashioned: on a capture where two libraries share an RVA it moves
    real cost onto the wrong symbol, silently and without an error."""
    wrong = report(
        load_fixture(V55),
        AddressOnlySidecar(pct.load_json_maybe_gz(sidecar_path_for(V55))),
    )
    wrong_self = parse_section(wrong, SELF)

    # Premise: the collision must actually be live in this fixture, or this
    # control proves nothing at all.
    assert wrong_self != EXPECTED_SELF, (
        "the address-only join produced the correct table -- the fixture does not "
        "actually contain a cross-library address collision, so this control is "
        "premise-false."
    )
    # And the specific damage: liba's cost is now filed under libb's symbol.
    assert "liba::alpha_symbol" not in wrong_self, wrong
    assert wrong_self == {"libb::beta_symbol": 100.0}, (
        f"expected all 100 units misattributed to libb, got {wrong_self}"
    )
    require_wrong("address-only join", lambda: _assert_correct_self(wrong_self))


def _assert_correct_self(self_time: dict[str, float]) -> None:
    assert self_time == EXPECTED_SELF


def control_collision_fixture_still_collides() -> None:
    """Guards the guard: assert the two fixture libraries really do publish
    different symbols at the same RVA. If a future edit de-duplicates the
    fixture, this fails here rather than turning the control above into a
    silent no-op."""
    syms = sidecar_for(V55)
    a = syms.resolve("liba", 0x1000)
    b = syms.resolve("libb", 0x1000)
    assert a == "liba::alpha_symbol", a
    assert b == "libb::beta_symbol", b
    assert a != b, "fixture libraries no longer disagree at RVA 0x1000"


# --------------------------------------------------------------------------
# 3. per-thread tables are per-thread
# --------------------------------------------------------------------------


def test_per_thread_tables_are_not_borrowed_across_threads() -> None:
    """In the <=55 layout every thread has its own `stringArray`/`funcTable`,
    so function indices mean nothing in another thread's tables. The worker
    thread's fixture reuses indices 0/1 for *different* symbols precisely so
    that borrowing the main thread's tables is detectable."""
    text = report(load_fixture(V55), sidecar_for(V55), thread="worker")
    assert "thread: 'worker'" in text, text
    self_time = parse_section(text, SELF)
    assert self_time == {"liba::gamma_symbol": 100.0}, text
    incl = parse_section(text, INCL)
    assert incl == {"fixture_worker_plain": 100.0, "liba::gamma_symbol": 100.0}, text
    # Nothing from the main thread may leak in.
    for leaked in ("liba::alpha_symbol", "libb::beta_symbol", "fixture_plain_main"):
        assert leaked not in incl, f"{leaked!r} leaked from the main thread: {text}"


def control_borrowing_thread_zero_tables_is_detectable() -> None:
    """Prove the assertion above can fail: resolve the worker thread's stacks
    against thread 0's tables, as a single shared table set would."""
    raw = load_fixture(V55)
    profile = pct.Profile(raw, sidecar_for(V55))
    wrong_tables = profile.tables_for(0)  # main thread's tables
    worker = raw["threads"][1]
    self_time, incl, total, _ = pct.compute_cost_tables(wrong_tables, worker)

    def assert_correct() -> None:
        assert self_time == {"liba::gamma_symbol": 100.0}, self_time

    require_wrong("worker samples against thread 0's tables", assert_correct)
    assert "liba::gamma_symbol" not in self_time, (
        f"borrowing thread 0's tables happened to give the right answer: {self_time}"
    )


# --------------------------------------------------------------------------
# 4. layout dispatch fails loudly rather than guessing
# --------------------------------------------------------------------------


def expect_exit(fn, *must_contain: str) -> None:
    try:
        fn()
    except SystemExit as e:
        msg = str(e)
        for needle in must_contain:
            assert needle in msg, f"error message {msg!r} does not mention {needle!r}"
        return
    raise AssertionError(f"expected SystemExit mentioning {must_contain!r}, got none")


def test_future_version_is_a_loud_error_naming_the_version() -> None:
    raw = load_fixture(V56)
    raw["preprocessedProfileVersion"] = pct.MAX_KNOWN_PROFILE_VERSION + 7
    expect_exit(
        lambda: pct.Profile(raw, None),
        str(pct.MAX_KNOWN_PROFILE_VERSION + 7),
        "MAX_KNOWN_PROFILE_VERSION",
    )


def test_version_contradicting_shape_is_a_loud_error() -> None:
    # per-thread version, but hoisted tables present
    raw = load_fixture(V55)
    raw["shared"] = load_fixture(V56)["shared"]
    expect_exit(lambda: pct.Profile(raw, None), "per-thread", "shared")

    # hoisted version, but no `shared` object
    raw2 = load_fixture(V56)
    del raw2["shared"]
    expect_exit(lambda: pct.Profile(raw2, None), "hoisted", "shared")


def test_non_integer_version_is_a_loud_error() -> None:
    raw = load_fixture(V56)
    raw["preprocessedProfileVersion"] = "56"
    expect_exit(lambda: pct.Profile(raw, None), "expected an integer")


def test_absent_version_reads_as_per_thread_not_unknown() -> None:
    """samply 0.13.1 emits no version key at all. Absent must mean
    per-thread, or every 0.13.1 capture is unreadable -- the original bug."""
    raw = load_fixture(V55)
    del raw["preprocessedProfileVersion"]
    layout, version = pct.detect_layout(raw)
    assert layout == pct.LAYOUT_PER_THREAD, layout
    assert version is None, version


# --------------------------------------------------------------------------
# 5. `prefix` and `prefixOffset` are different things
# --------------------------------------------------------------------------


def control_prefix_read_as_offset_is_a_silent_wrong_answer() -> None:
    """The reason the two encodings are dispatched on key presence and never
    "tried": relabel the v55 fixture's `prefix` column as `prefixOffset` --
    the exact confusion -- and the script does **not** crash. It emits a
    table that looks entirely plausible.

    Measured here, and worth stating precisely because it decides how this
    file asserts: the **self-time table comes out byte-identical** (leaves
    are unaffected -- a leaf is a leaf whichever way you walk upward), while
    the **inclusive table silently loses the root frame** entirely. A test
    that checked only self time would therefore be vacuous against this
    whole class of bug. The cycle guard does not fire either: `prefix`
    `[null, 0, 1]` becomes offsets `[0, 0, 1]`, and offset 0 reads as a
    legitimate root."""
    raw = load_fixture(V55)
    st = raw["threads"][0]["stackTable"]
    st["prefixOffset"] = [0 if p is None else p for p in st["prefix"]]
    del st["prefix"]
    text = report(raw, sidecar_for(V55))

    # Self time is identical -- this is the vacuity trap, asserted so it is
    # on the record rather than merely believed.
    assert parse_section(text, SELF) == EXPECTED_SELF, (
        "self time was expected to survive the confusion unchanged; if this now "
        "differs, re-derive what this control proves."
    )
    # Inclusive time is where the damage is: the root frame vanishes.
    wrong_incl = parse_section(text, INCL)
    assert "fixture_plain_main" not in wrong_incl, (
        f"expected the root frame to be lost, got {wrong_incl}"
    )
    require_wrong("prefix relabelled as prefixOffset", lambda: _assert_correct_incl(wrong_incl))


def _assert_correct_incl(incl: dict[str, float]) -> None:
    assert incl == EXPECTED_INCL


def test_cycling_stack_table_is_caught() -> None:
    """The cycle guard itself, on input that does cycle."""
    raw = load_fixture(V55)
    st = raw["threads"][0]["stackTable"]
    del st["prefix"]
    st["prefixOffset"] = [0, 2, 1]  # stack 1 -> parent -1... walk revisits
    st["prefixOffset"] = [0, -1, 1]  # stack 1's parent is stack 2, whose parent is 1
    expect_exit(lambda: report(raw, sidecar_for(V55)), "cycles")


def test_missing_both_stack_encodings_is_a_loud_error() -> None:
    raw = load_fixture(V55)
    st = raw["threads"][0]["stackTable"]
    del st["prefix"]
    expect_exit(lambda: report(raw, sidecar_for(V55)), "prefix", "prefixOffset")


def test_missing_table_names_the_table_and_where() -> None:
    raw = load_fixture(V55)
    del raw["threads"][0]["funcTable"]
    expect_exit(lambda: report(raw, sidecar_for(V55)), "funcTable", 'threads"][0]')


# --------------------------------------------------------------------------
# 6. sidecar path derivation
# --------------------------------------------------------------------------


def test_sidecar_path_matches_samply_naming() -> None:
    """samply's `with_extension("syms.json")` on `p.json.gz` gives
    `p.json.syms.json`, measured against 0.13.1. Both spellings resolve."""
    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        (d / "p.json.gz").write_bytes(b"{}")
        # nothing on disk yet -> first candidate, samply's real spelling
        assert pct.derive_syms_path(d / "p.json.gz").name == "p.json.syms.json"
        # the samply-produced spelling wins when present
        (d / "p.json.syms.json").write_bytes(b"{}")
        assert pct.derive_syms_path(d / "p.json.gz").name == "p.json.syms.json"
        # the alternative spelling is still tolerated
        (d / "q.json.gz").write_bytes(b"{}")
        (d / "q.syms.json").write_bytes(b"{}")
        assert pct.derive_syms_path(d / "q.json.gz").name == "q.syms.json"
        # and the real fixture resolves without an explicit --syms: samply
        # named it samply-0.13.1-real.json.syms.json, dropping only the .gz
        assert pct.derive_syms_path(FIXTURES / REAL).name == "samply-0.13.1-real.json.syms.json"
        assert pct.derive_syms_path(FIXTURES / REAL).exists()


def test_duplicate_debug_name_in_sidecar_does_not_corrupt_indices() -> None:
    raw = pct.load_json_maybe_gz(sidecar_path_for(V55))
    raw["data"].append(copy.deepcopy(raw["data"][0]))
    raw["data"][-1]["symbol_table"] = [{"rva": 4096, "size": 32, "symbol": 2}]
    syms = pct.SymsSidecar(raw)
    assert syms.resolve("liba", 0x1000) == "liba::alpha_symbol"


def main() -> int:
    checks = [
        ("real capture parses and attributes to the right symbols", test_real_capture_parses_and_attributes),
        ("CONTROL: no sidecar -> raw addresses", control_real_capture_without_sidecar_is_raw_addresses),
        ("CONTROL: corrupted sidecar symbol breaks attribution", control_real_capture_corrupted_symbol_table),
        ("CONTROL: emptied samples report no table", control_real_capture_truncated_samples),
        ("collision fixture, per-thread layout (v55)", test_collision_fixture_per_thread_v55),
        ("collision fixture, hoisted layout (v56)", test_collision_fixture_shared_v56),
        ("CONTROL: fixture libraries really do collide at 0x1000", control_collision_fixture_still_collides),
        ("CONTROL: address-only join misattributes 100 units", control_address_only_join_misattributes),
        ("per-thread tables are not borrowed across threads", test_per_thread_tables_are_not_borrowed_across_threads),
        ("CONTROL: borrowing thread 0's tables is detectable", control_borrowing_thread_zero_tables_is_detectable),
        ("future version is a loud error naming the version", test_future_version_is_a_loud_error_naming_the_version),
        ("version contradicting shape is a loud error", test_version_contradicting_shape_is_a_loud_error),
        ("non-integer version is a loud error", test_non_integer_version_is_a_loud_error),
        ("absent version reads as per-thread", test_absent_version_reads_as_per_thread_not_unknown),
        ("CONTROL: prefix read as offset is a silent wrong answer", control_prefix_read_as_offset_is_a_silent_wrong_answer),
        ("a genuinely cycling stackTable is caught", test_cycling_stack_table_is_caught),
        ("missing both stack encodings is a loud error", test_missing_both_stack_encodings_is_a_loud_error),
        ("missing table names the table and where", test_missing_table_names_the_table_and_where),
        ("sidecar path matches samply naming", test_sidecar_path_matches_samply_naming),
        ("duplicate debugName does not corrupt indices", test_duplicate_debug_name_in_sidecar_does_not_corrupt_indices),
    ]
    for name, fn in checks:
        check(name, fn)
    print()
    if FAILURES:
        print(f"{len(FAILURES)} of {len(checks)} checks FAILED\n")
        for f in FAILURES:
            print(f)
        return 1
    print(f"all {len(checks)} checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
