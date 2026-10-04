#!/usr/bin/env python3
"""Build and validate an append-only canonical identity manifest from reports."""

import argparse
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
VERSIONS = ("26.2", "26.3")
DOMAINS = ("blocks", "block_states", "items")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def read_report(path):
    data = path.read_bytes()
    return json.loads(data, object_pairs_hook=unique_object), hashlib.sha256(data).hexdigest()


def ordered_keys(pairs, label):
    by_id = {}
    seen = set()
    for key, raw in pairs:
        if type(raw) is not int or raw < 0 or raw in by_id or key in seen:
            raise ValueError(f"{label}: duplicate key or invalid/duplicate ID: {key} / {raw}")
        by_id[raw] = key
        seen.add(key)
    if not by_id or set(by_id) != set(range(len(by_id))):
        raise ValueError(f"{label}: IDs must be nonempty and dense from zero")
    return [by_id[raw] for raw in range(len(by_id))]


def state_key(name, properties):
    if not isinstance(properties, dict) or any(
        not isinstance(key, str) or not isinstance(value, str)
        for key, value in properties.items()
    ):
        raise ValueError(f"{name}: state properties must be string pairs")
    suffix = ",".join(f"{key}={value}" for key, value in sorted(properties.items()))
    return f"{name}[{suffix}]" if suffix else name


def registry_union_names(registries_by_version, registry):
    ordered = {}
    for version in VERSIONS:
        entries = registries_by_version[version][registry]["entries"]
        ordered[version] = ordered_keys(((name, entry["protocol_id"]) for name, entry in entries.items()),
                                        f"{registry}/{version}")
    base, latest = (ordered[version] for version in VERSIONS)
    seen = set(base)
    if not seen <= set(latest):
        raise ValueError(f"{registry}: latest report removed a base identity")
    return base + [name for name in latest if name not in seen]


def block_identities(blocks):
    identities = {}
    for name, block in blocks.items():
        states = block["states"]
        if not states or any(type(state.get("default", False)) is not bool for state in states):
            raise ValueError(f"{name}: states must be nonempty with boolean default flags")
        defaults = [state for state in states if state.get("default", False)]
        if len(defaults) != 1:
            raise ValueError(f"{name}: expected exactly one default state")
        identities[name] = {
            "states": [state_key(name, state.get("properties", {})) for state in states],
            "default_state": state_key(name, defaults[0].get("properties", {})),
        }
    return identities


def load_source(directory):
    blocks, block_digest = read_report(directory / "blocks.json")
    registries, registry_digest = read_report(directory / "registries.json")
    result = {}
    for domain, registry in (("blocks", "minecraft:block"), ("items", "minecraft:item")):
        result[domain] = ordered_keys(
            ((key, value["protocol_id"]) for key, value in registries[registry]["entries"].items()),
            registry,
        )
    if set(blocks) != set(result["blocks"]):
        raise ValueError("blocks.json and the block registry have different names")
    result["block_states"] = ordered_keys(
        (
            (state_key(name, state.get("properties", {})), state["id"])
            for name, block in blocks.items()
            for state in block["states"]
        ),
        "block states",
    )
    result["block_identities"] = block_identities(blocks)
    result["source_sha256"] = {
        "blocks.json": block_digest,
        "registries.json": registry_digest,
    }
    return result


def validate_domain(domain, manifest, sources):
    keys = manifest["keys"]
    if not keys or len(set(keys)) != len(keys):
        raise ValueError(f"{domain}: canonical keys must be nonempty and unique")
    base = sources[VERSIONS[0]][domain]
    latest = sources[VERSIONS[1]][domain]
    if keys[:len(base)] != base:
        raise ValueError(f"{domain}: canonical base prefix changed")
    base_names = set(base)
    if not base_names <= set(latest):
        raise ValueError(f"{domain}: latest report removed a base identity")
    if keys[len(base):] != [key for key in latest if key not in base_names]:
        raise ValueError(f"{domain}: additions must follow latest wire order exactly")
    canonical = {key: index for index, key in enumerate(keys)}
    for version in VERSIONS:
        source = sources[version][domain]
        wire = {key: index for index, key in enumerate(source)}
        mapping = manifest["versions"][version]
        forward = mapping["wire_to_canonical"]
        reverse = mapping["canonical_to_wire"]
        if len(forward) != len(source) or len(reverse) != len(keys):
            raise ValueError(f"{domain}/{version}: mapping length differs from its domain")
        for wire_id, key in enumerate(source):
            value = forward[wire_id]
            if type(value) is not int or value != canonical[key]:
                raise ValueError(f"{domain}/{version}: wrong ingress at wire ID {wire_id} ({key})")
        for canonical_id, key in enumerate(keys):
            value = reverse[canonical_id]
            expected = wire.get(key)
            if value != expected or (value is not None and type(value) is not int):
                raise ValueError(f"{domain}/{version}: wrong egress at canonical ID {canonical_id} ({key})")
    if domain == "block_states":
        # Numeric property lookup currently depends on one span per block.
        closed = set()
        previous = None
        for key in keys:
            block = key.split("[", 1)[0]
            if block != previous:
                if block in closed:
                    raise ValueError(f"block_states: {block} needs multiple canonical spans")
                if previous is not None:
                    closed.add(previous)
                previous = block


def validate_manifest(manifest, sources):
    if manifest["schema_version"] != 1 or manifest["base_version"] != VERSIONS[0]:
        raise ValueError("unsupported canonical manifest schema or base")
    if set(manifest["domains"]) != set(DOMAINS):
        raise ValueError("canonical manifest domain set differs")
    if manifest["source_sha256"] != {
        version: sources[version]["source_sha256"] for version in VERSIONS
    }:
        raise ValueError("canonical manifest report provenance differs")
    for domain in DOMAINS:
        validate_domain(domain, manifest["domains"][domain], sources)


def build_manifest(sources):
    manifest = {
        "schema_version": 1,
        "base_version": VERSIONS[0],
        "source_sha256": {version: sources[version]["source_sha256"] for version in VERSIONS},
        "domains": {},
    }
    for domain in DOMAINS:
        base = sources[VERSIONS[0]][domain]
        seen = set(base)
        keys = base + [key for key in sources[VERSIONS[1]][domain] if key not in seen]
        canonical = {key: index for index, key in enumerate(keys)}
        versions = {}
        for version in VERSIONS:
            wire = {key: index for index, key in enumerate(sources[version][domain])}
            versions[version] = {
                "wire_to_canonical": [canonical[key] for key in sources[version][domain]],
                "canonical_to_wire": [wire.get(key) for key in keys],
            }
        manifest["domains"][domain] = {"keys": keys, "versions": versions}
    validate_manifest(manifest, sources)
    return manifest


def serialize(manifest):
    return json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-reports", type=Path, default=ROOT / ".cache/mc/26.2/generated/reports")
    parser.add_argument("--latest-reports", type=Path, default=ROOT / ".cache/mc/26.3/generated/reports")
    destination = parser.add_mutually_exclusive_group()
    destination.add_argument("--output", type=Path, help="create a new manifest; refuses to overwrite")
    destination.add_argument("--check", type=Path, help="verify an existing manifest byte for byte")
    args = parser.parse_args()
    sources = {
        VERSIONS[0]: load_source(args.base_reports),
        VERSIONS[1]: load_source(args.latest_reports),
    }
    manifest = build_manifest(sources)
    encoded = serialize(manifest)
    if args.output:
        with args.output.open("x", encoding="utf-8") as output:
            output.write(encoded)
    if args.check and args.check.read_text(encoding="utf-8") != encoded:
        raise SystemExit(f"canonical manifest differs: {args.check}")
    for domain in DOMAINS:
        print(f"{domain}: {len(sources[VERSIONS[0]][domain])} base + "
              f"{len(manifest['domains'][domain]['keys']) - len(sources[VERSIONS[0]][domain])} added")
    print(f"manifest_sha256: {hashlib.sha256(encoded.encode()).hexdigest()}")


if __name__ == "__main__":
    main()
