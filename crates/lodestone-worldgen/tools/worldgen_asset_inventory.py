#!/usr/bin/env python3
"""Summarize versioned worldgen datapack documents from official server jars.

The output is an inventory, not a copy of the input assets. It keeps document
counts, schema discriminator counts, cross-document references, bounded delta
examples, source digests, and a handful of independently checked witnesses.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import zipfile
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any


DEFAULT_JARS = {
    "26.2": Path(".cache/mc/26.2/versions/26.2/server-26.2.jar"),
    "26.3": Path(".cache/mc/26.3/versions/26.3/server-26.3.jar"),
}
DEFAULT_REPORTS = {
    "26.2": Path(".cache/mc/26.2/generated/reports/registries.json"),
    "26.3": Path(".cache/mc/26.3/generated/reports/registries.json"),
}
DOC_PREFIX = "data/minecraft/"
DOC_ROOTS = ("worldgen/", "dimension_type/", "dimension/")
DISC_KEYS = {"type", "predicate_type", "processor_type", "element_type"}
MAX_EXAMPLES = 12


class InventoryError(Exception):
    pass


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def doc_kind(path: str) -> str:
    relative = path[len(DOC_PREFIX) :]
    if relative.startswith(("dimension_type/", "dimension/")):
        return relative.split("/", 1)[0]
    pieces = relative.split("/")
    return "/".join(pieces[:2]) if len(pieces) > 1 else pieces[0]


def resource_id(path: str) -> str:
    relative = path[len(DOC_PREFIX) :]
    if relative.endswith(".json"):
        relative = relative[:-5]
    if relative.startswith("worldgen/"):
        return "minecraft:" + relative[len("worldgen/") :].split("/", 1)[1]
    for root in ("dimension_type/", "dimension/"):
        if relative.startswith(root):
            return "minecraft:" + relative[len(root) :]
    return "minecraft:" + relative


def load_archive(label: str, jar_path: Path) -> dict[str, Any]:
    if not jar_path.is_file():
        raise InventoryError(f"missing {label} server jar: {jar_path}")
    digest = hashlib.sha256()
    with jar_path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    documents: dict[str, Any] = {}
    file_digests: dict[str, str] = {}
    try:
        with zipfile.ZipFile(jar_path) as archive:
            names = sorted(
                name
                for name in archive.namelist()
                if name.startswith(DOC_PREFIX)
                and name.endswith(".json")
                and any(name.startswith(DOC_PREFIX + root) for root in DOC_ROOTS)
            )
            for name in names:
                raw = archive.read(name)
                try:
                    documents[name] = json.loads(raw)
                except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                    raise InventoryError(f"invalid JSON document {name} in {label}: {exc}") from exc
                file_digests[name] = sha256(raw)
    except zipfile.BadZipFile as exc:
        raise InventoryError(f"invalid server jar {jar_path}: {exc}") from exc
    if not documents:
        raise InventoryError(f"no worldgen/dimension JSON documents found in {jar_path}")
    return {
        "jar": str(jar_path),
        "jar_sha256": digest.hexdigest(),
        "documents": documents,
        "file_digests": file_digests,
    }


def load_registry_report(version: str, path: Path) -> dict[str, Any]:
    if not path.is_file():
        raise InventoryError(f"missing {version} registry report: {path}")
    raw = path.read_bytes()
    try:
        report = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise InventoryError(f"invalid registry report {path}: {exc}") from exc
    registries = {}
    for key, value in sorted(report.items()):
        if not key.startswith("minecraft:worldgen/"):
            continue
        entries = value.get("entries")
        if not isinstance(entries, dict):
            raise InventoryError(f"registry report entry {key} has no entries object")
        identifiers = sorted(entries)
        protocol_ids = {
            identifier: entries[identifier].get("protocol_id")
            for identifier in identifiers
        }
        canonical = "\n".join(f"{key} {protocol_ids[key]}" for key in identifiers).encode()
        registries[key] = {
            "entry_count": len(identifiers),
            "entries_sha256": sha256(canonical),
            "entry_ids": identifiers,
        }
    if not registries:
        raise InventoryError(f"no worldgen registries found in {path}")
    return {"path": str(path), "sha256": sha256(raw), "registries": registries}


def walk(value: Any, path: str = "$", depth: int = 0):
    if depth > 80:
        raise InventoryError(f"JSON nesting exceeds inventory limit at {path}")
    if isinstance(value, dict):
        for key, child in sorted(value.items()):
            child_path = f"{path}.{key}"
            yield key, child, child_path
            yield from walk(child, child_path, depth + 1)
    elif isinstance(value, list):
        for child in value:
            yield "[]", child, path + "[]"
            yield from walk(child, path + "[]", depth + 1)


def archive_summary(archive: dict[str, Any]) -> dict[str, Any]:
    docs = archive["documents"]
    ids_by_kind: dict[str, set[str]] = defaultdict(set)
    paths_by_id: dict[str, set[str]] = defaultdict(set)
    counts = Counter()
    discriminators: dict[str, Counter] = defaultdict(Counter)
    ref_counts: Counter = Counter()
    ref_examples: dict[str, set[str]] = defaultdict(set)
    state_forms: Counter = Counter()
    numeric_state_ordinals = 0
    ordinal_keys = {"state_id", "block_state_id", "block_state_ordinal"}

    for path, document in docs.items():
        kind = doc_kind(path)
        identifier = resource_id(path)
        ids_by_kind[kind].add(identifier)
        paths_by_id[identifier].add(kind)
        counts[kind] += 1

    for path, document in docs.items():
        source_kind = doc_kind(path)
        if source_kind.startswith("worldgen/"):
            for key, child, _ in walk(document):
                if isinstance(child, dict):
                    if isinstance(child.get("Name"), str):
                        state_forms["Name object"] += 1
                    if isinstance(child.get("id"), str) and child["id"].startswith("minecraft:"):
                        state_forms["id plus properties" if "properties" in child else "namespaced id"] += 1
                if key in ordinal_keys and isinstance(child, int) and not isinstance(child, bool):
                    numeric_state_ordinals += 1
        for key, child, pointer in walk(document):
            if key in DISC_KEYS and isinstance(child, str):
                discriminators[source_kind][f"{key}={child}"] += 1
            if isinstance(child, str) and child in paths_by_id:
                targets = ",".join(sorted(paths_by_id[child]))
                ref_key = f"{source_kind} -> {targets} via {key}"
                ref_counts[ref_key] += 1
                if len(ref_examples[ref_key]) < MAX_EXAMPLES:
                    ref_examples[ref_key].add(child)

    return {
        "document_counts": dict(sorted(counts.items())),
        "document_ids": {kind: sorted(ids) for kind, ids in sorted(ids_by_kind.items())},
        "block_state_encodings": {
            "symbolic_form_counts": dict(sorted(state_forms.items())),
            "explicit_numeric_ordinal_fields": numeric_state_ordinals,
        },
        "discriminators": {
            kind: dict(counter.most_common()) for kind, counter in sorted(discriminators.items())
        },
        "references": [
            {"edge": edge, "occurrences": total, "examples": sorted(ref_examples[edge])}
            for edge, total in sorted(ref_counts.items())
        ],
    }


def bounded_delta(old: set[str], new: set[str]) -> dict[str, Any]:
    added, removed = sorted(new - old), sorted(old - new)
    return {
        "added_count": len(added),
        "added_examples": added[:MAX_EXAMPLES],
        "removed_count": len(removed),
        "removed_examples": removed[:MAX_EXAMPLES],
        "examples_truncated": len(added) > MAX_EXAMPLES or len(removed) > MAX_EXAMPLES,
    }


def make_delta(left: dict[str, Any], right: dict[str, Any]) -> dict[str, Any]:
    a, b = left["summary"], right["summary"]
    all_kinds = sorted(set(a["document_counts"]) | set(b["document_counts"]))
    kinds = {
        kind: {
            "count_26_2": a["document_counts"].get(kind, 0),
            "count_26_3": b["document_counts"].get(kind, 0),
            "documents": bounded_delta(
                set(a["document_ids"].get(kind, [])), set(b["document_ids"].get(kind, []))
            ),
            "discriminators": bounded_delta(
                set(a["discriminators"].get(kind, {})), set(b["discriminators"].get(kind, {}))
            ),
        }
        for kind in all_kinds
    }
    return {"kinds": kinds}


def make_registry_delta(left: dict[str, Any], right: dict[str, Any]) -> dict[str, Any]:
    a, b = left["registries"], right["registries"]
    names = sorted(set(a) | set(b))
    return {
        name: {
            "count_26_2": a.get(name, {}).get("entry_count", 0),
            "count_26_3": b.get(name, {}).get("entry_count", 0),
            "entries": bounded_delta(
                set(a.get(name, {}).get("entry_ids", [])),
                set(b.get(name, {}).get("entry_ids", [])),
            ),
            "entries_sha256_26_2": a.get(name, {}).get("entries_sha256"),
            "entries_sha256_26_3": b.get(name, {}).get("entries_sha256"),
        }
        for name in names
    }


def find_witness(documents: dict[str, Any], path_suffix: str, selectors: tuple[str, ...]) -> dict[str, Any]:
    matches = [path for path in documents if path.endswith(path_suffix)]
    if len(matches) != 1:
        raise InventoryError(f"witness expected one *{path_suffix}, found {len(matches)}")
    path = matches[0]
    value: Any = documents[path]
    for selector in selectors:
        if selector == "[]":
            if not isinstance(value, list) or not value:
                raise InventoryError(f"witness selector [] absent in {path}")
            value = value[0]
        else:
            if not isinstance(value, dict) or selector not in value:
                raise InventoryError(f"witness selector {selector!r} absent in {path}")
            value = value[selector]
    return {"path": path, "selectors": list(selectors), "observed": value}


def self_check(version: str, archive: dict[str, Any]) -> dict[str, str]:
    # Literal paths and expected values independently witness the archive parser.
    if version == "26.2":
        witness = find_witness(
            archive["documents"], "/noise_settings/overworld.json", ("surface_rule", "type")
        )
        expected = "minecraft:sequence"
        label = "26.2 overworld surface rule"
    else:
        witness = find_witness(
            archive["documents"], "/noise_settings/overworld.json", ("material_rule",)
        )
        expected = "minecraft:overworld"
        label = "26.3 overworld material rule reference"
    if witness["observed"] != expected:
        raise InventoryError(f"{version} witness changed: {label} expected {expected!r}")
    return {"witness": f"passed: {label} at {witness['path']} matched {expected}"}


def inventory(jars: dict[str, Path], report_paths: dict[str, Path]) -> dict[str, Any]:
    loaded = {version: load_archive(version, path) for version, path in jars.items()}
    for item in loaded.values():
        item["summary"] = archive_summary(item)
    checks = {
        version: self_check(version, item)
        for version, item in sorted(loaded.items())
    }
    reports = {
        version: load_registry_report(version, path)
        for version, path in report_paths.items()
    }
    return {
        "format": "lodestone-worldgen-asset-inventory/v1",
        "sources": {
            version: {
                "jar": item["jar"],
                "jar_sha256": item["jar_sha256"],
                "document_count": len(item["documents"]),
                "document_manifest_sha256": sha256(
                    "\n".join(f"{p} {item['file_digests'][p]}" for p in sorted(item["file_digests"])).encode()
                ),
                "summary": item["summary"],
            }
            for version, item in sorted(loaded.items())
        },
        "worldgen_registry_reports": {
            version: {
                "path": report["path"],
                "sha256": report["sha256"],
                "registries": {
                    name: {
                        "entry_count": value["entry_count"],
                        "entries_sha256": value["entries_sha256"],
                    }
                    for name, value in sorted(report["registries"].items())
                },
            }
            for version, report in sorted(reports.items())
        },
        "delta_26_2_to_26_3": make_delta(loaded["26.2"], loaded["26.3"]),
        "registry_delta_26_2_to_26_3": make_registry_delta(reports["26.2"], reports["26.3"]),
        "checks": checks,
        "failure_policy": {
            "missing_document": "fail inventory generation",
            "malformed_json_or_excessive_nesting": "fail inventory generation",
            "unknown_discriminator": "typed runtime must return an explicit unsupported-capability error; never default or silently skip",
            "unresolved_resource_reference": "typed runtime must return an explicit unresolved-reference error",
        },
        "runtime_note": "This inventory is build-time evidence only; no string lookup is proposed for the runtime generation path.",
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar-262", type=Path, default=DEFAULT_JARS["26.2"])
    parser.add_argument("--jar-263", type=Path, default=DEFAULT_JARS["26.3"])
    parser.add_argument("--report-262", type=Path, default=DEFAULT_REPORTS["26.2"])
    parser.add_argument("--report-263", type=Path, default=DEFAULT_REPORTS["26.3"])
    parser.add_argument("--out", type=Path, help="write bounded JSON inventory here; stdout if omitted")
    args = parser.parse_args()
    try:
        result = inventory(
            {"26.2": args.jar_262, "26.3": args.jar_263},
            {"26.2": args.report_262, "26.3": args.report_263},
        )
    except (InventoryError, OSError) as exc:
        print(f"worldgen asset inventory: {exc}", file=sys.stderr)
        return 2
    encoded = json.dumps(result, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(encoded, encoding="utf-8")
    else:
        sys.stdout.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
