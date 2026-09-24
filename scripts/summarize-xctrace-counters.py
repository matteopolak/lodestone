#!/usr/bin/env python3

import argparse
import collections
import re
import shutil
import subprocess
from pathlib import Path
import xml.etree.ElementTree as ET


def resolve(element, by_id):
    seen = set()
    while element is not None and element.get("ref"):
        reference = element.get("ref")
        if reference in seen:
            return None
        seen.add(reference)
        element = by_id.get(reference)
    return element


def counter_values(row, by_id):
    element = resolve(row.find("pmc-events"), by_id)
    if element is None or not element.text:
        return None
    try:
        values = tuple(int(value) for value in element.text.split())
    except ValueError:
        return None
    return values or None


def event_names(toc_path, explicit):
    if explicit:
        names = tuple(name.strip() for name in re.split(r"[,;]", explicit) if name.strip())
        if names:
            return names
    if toc_path:
        root = ET.parse(toc_path).getroot()
        for table in root.iter("table"):
            if table.get("schema") == "counters-profile" and table.get("pmc-events"):
                names = tuple(table.get("pmc-events", "").split())
                if names:
                    return names
    return ("Cycles", "Instructions")


def phase_markers(path):
    if path is None:
        return []
    markers = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.startswith("STRICT_WORLDGEN "):
            continue
        fields = dict(
            item.split("=", 1)
            for item in line.removeprefix("STRICT_WORLDGEN ").split()
            if "=" in item
        )
        if "metric" in fields and "phase" in fields:
            markers.append(fields)
    return markers


def display_event(name):
    return name.lower().replace(" ", "_").replace("/", "_")


def event_index(names, wanted):
    wanted = wanted.lower()
    return next((index for index, name in enumerate(names) if name.lower() == wanted), None)


def stack_frames(row, by_id):
    tagged = resolve(row.find("tagged-backtrace"), by_id)
    if tagged is None:
        return []
    backtrace = resolve(tagged.find("backtrace"), by_id)
    if backtrace is None:
        return []
    frames = []
    for raw in backtrace.findall("frame"):
        frame = resolve(raw, by_id)
        if frame is not None:
            frames.append(frame.get("name", frame.get("fmt", "<address>")))
    return frames


def demangle(names):
    rustfilt = shutil.which("rustfilt")
    if rustfilt is None or not names:
        return {name: name for name in names}
    result = subprocess.run(
        [rustfilt],
        input="\n".join(names),
        text=True,
        capture_output=True,
        check=True,
    )
    output = result.stdout.splitlines()
    if len(output) != len(names):
        return {name: name for name in names}
    return dict(zip(names, output))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("xml")
    parser.add_argument("--process", default="bench_worldgen")
    parser.add_argument("--limit", type=int, default=25)
    parser.add_argument(
        "--toc",
        help="optional xctrace TOC; derives the event names for vectors wider than Cycles/Instructions",
    )
    parser.add_argument(
        "--events",
        help="comma- or semicolon-separated PMU event names, overriding --toc",
    )
    parser.add_argument(
        "--phase-log",
        help="target stdout containing STRICT_WORLDGEN phase reports",
    )
    args = parser.parse_args()

    root = ET.parse(args.xml).getroot()
    by_id = {element.get("id"): element for element in root.iter() if element.get("id")}
    names = event_names(args.toc, args.events)
    exclusive = collections.defaultdict(lambda: [0, 0, 0])
    inclusive = collections.defaultdict(lambda: [0, 0, 0])
    totals = [0] * len(names)
    observed_width = 0
    samples = 0

    for row in root.iter("row"):
        process = resolve(row.find("process"), by_id)
        if process is None or args.process not in process.get("fmt", ""):
            continue
        counters = counter_values(row, by_id)
        if counters is None:
            continue
        observed_width = max(observed_width, len(counters))
        if len(counters) > len(totals):
            first_unknown = len(names) + 1
            names += tuple(
                f"Counter{index}"
                for index in range(first_unknown, first_unknown + len(counters) - len(names))
            )
            totals.extend([0] * (len(counters) - len(totals)))
        for index, value in enumerate(counters[: len(totals)]):
            totals[index] += value
        samples += 1
        frames = stack_frames(row, by_id)
        if not frames:
            continue
        cycle_index = event_index(names, "Cycles")
        instruction_index = event_index(names, "Instructions")
        cycles = counters[cycle_index] if cycle_index is not None and cycle_index < len(counters) else 0
        instructions = counters[instruction_index] if instruction_index is not None and instruction_index < len(counters) else 0
        leaf = frames[0]
        exclusive[leaf][0] += cycles
        exclusive[leaf][1] += instructions
        exclusive[leaf][2] += 1
        for frame in set(frames):
            inclusive[frame][0] += cycles
            inclusive[frame][1] += instructions
            inclusive[frame][2] += 1

    symbol_names = set(exclusive) | set(inclusive)
    display = demangle(sorted(symbol_names))
    cycle_index = event_index(names, "Cycles")
    instruction_index = event_index(names, "Instructions")
    total_cycles = totals[cycle_index] if cycle_index is not None and cycle_index < observed_width else None
    total_instructions = totals[instruction_index] if instruction_index is not None and instruction_index < observed_width else None
    ipc = total_instructions / total_cycles if total_cycles else None
    cycles_text = "unavailable" if total_cycles is None else str(total_cycles)
    instructions_text = "unavailable" if total_instructions is None else str(total_instructions)
    ipc_text = "unavailable" if ipc is None else f"{ipc:.3f}"
    print(
        f"process={args.process} samples={samples} cycles={cycles_text} "
        f"instructions={instructions_text} ipc={ipc_text}"
    )
    print("counters:")
    for index, (name, value) in enumerate(zip(names, totals)):
        rendered = value if index < observed_width else "unavailable"
        print(f"  {display_event(name)}={rendered}")
    for title, table in (("exclusive leaves", exclusive), ("inclusive frames", inclusive)):
        print(f"\n{title}:")
        ranked = sorted(table.items(), key=lambda item: item[1][1], reverse=True)
        for name, (cycles, instructions, count) in ranked[: args.limit]:
            print(
                f"  {instructions:14d} ins {cycles:14d} cyc "
                f"{count:6d} samples  {display[name]}"
            )

    markers = phase_markers(Path(args.phase_log)) if args.phase_log else []
    if markers:
        print("\nphase markers:")
        for marker in markers:
            values = " ".join(
                f"{key}={marker[key]}"
                for key in ("metric", "phase", "layout", "batch_size", "columns", "elapsed_ms", "instructions", "cycles")
                if key in marker
            )
            print(f"  {values}")


if __name__ == "__main__":
    main()
