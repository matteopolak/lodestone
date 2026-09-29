#!/usr/bin/env python3
"""Stage canonical identity columns without compiling or changing runtime tables."""

import argparse
import copy
import hashlib
from pathlib import Path

import canonical_census as census


SCOPES = ("base", "union")


def digest(value):
    return hashlib.sha256(census.serialize(value).encode()).hexdigest()


def selected_versions(scope):
    if scope not in SCOPES:
        raise ValueError(f"unknown identity scope: {scope}")
    return census.VERSIONS[:1] if scope == "base" else census.VERSIONS


def build_bundle(manifest, sources, scope="union"):
    census.validate_manifest(manifest, sources)
    versions = selected_versions(scope)
    domains = {}
    for name in census.DOMAINS:
        source = manifest["domains"][name]
        count = len(sources[census.VERSIONS[0]][name]) if scope == "base" else len(source["keys"])
        domains[name] = {
            "keys": source["keys"][:count],
            "versions": {
                version: {
                    "wire_to_canonical": source["versions"][version]["wire_to_canonical"][:],
                    "canonical_to_wire": source["versions"][version]["canonical_to_wire"][:count],
                }
                for version in versions
            },
        }
    blocks = domains["blocks"]
    states = domains["block_states"]
    block_ids = {key: index for index, key in enumerate(blocks["keys"])}
    state_ids = {key: index for index, key in enumerate(states["keys"])}
    states["block_ids"] = [block_ids[key.split("[", 1)[0]] for key in states["keys"]]
    spans = [[] for _ in blocks["keys"]]
    for state_id, block_id in enumerate(states["block_ids"]):
        spans[block_id].append(state_id)
    blocks["state_spans"] = [[ids[0], len(ids)] for ids in spans]
    for version in versions:
        identities = sources[version]["block_identities"]
        blocks["versions"][version]["default_states"] = [
            state_ids[identities[key]["default_state"]] if key in identities else None
            for key in blocks["keys"]
        ]
    bundle = {
        "schema_version": 1,
        "base_version": manifest["base_version"],
        "scope": scope,
        "census_sha256": digest(manifest),
        "source_sha256": copy.deepcopy(manifest["source_sha256"]),
        "domains": domains,
    }
    validate_bundle(bundle, manifest, sources)
    return bundle


def require_equal(actual, expected, label):
    if census.serialize(actual) != census.serialize(expected):
        raise ValueError(f"identity bundle: {label} differs")


def validate_bundle(bundle, manifest, sources):
    census.validate_manifest(manifest, sources)
    versions = selected_versions(bundle["scope"])
    require_equal(bundle["schema_version"], 1, "schema")
    require_equal(bundle["base_version"], manifest["base_version"], "base version")
    require_equal(bundle["census_sha256"], digest(manifest), "census provenance")
    require_equal(bundle["source_sha256"], manifest["source_sha256"], "report provenance")
    require_equal(sorted(bundle["domains"]), sorted(census.DOMAINS), "domains")
    for name in census.DOMAINS:
        domain = bundle["domains"][name]
        original = manifest["domains"][name]
        keys = sources[census.VERSIONS[0]][name] if bundle["scope"] == "base" else original["keys"]
        require_equal(domain["keys"], keys, f"{name} keys")
        require_equal(sorted(domain["versions"]), sorted(versions), f"{name} versions")
        for version in versions:
            mapping = domain["versions"][version]
            expected = original["versions"][version]
            require_equal(mapping["wire_to_canonical"], expected["wire_to_canonical"], f"{name}/{version} ingress")
            require_equal(mapping["canonical_to_wire"], expected["canonical_to_wire"][:len(keys)], f"{name}/{version} egress")
    blocks = bundle["domains"]["blocks"]
    states = bundle["domains"]["block_states"]
    require_equal(len(blocks["state_spans"]), len(blocks["keys"]), "block span count")
    require_equal(len(states["block_ids"]), len(states["keys"]), "state owner count")
    state_ids = {key: index for index, key in enumerate(states["keys"])}
    memberships = {key: set() for key in blocks["keys"]}
    for version in versions:
        identities = sources[version]["block_identities"]
        require_equal(sorted(identities), sorted(sources[version]["blocks"]), f"{version} report block coverage")
        defaults = blocks["versions"][version]["default_states"]
        require_equal(len(defaults), len(blocks["keys"]), f"{version} default count")
        for block_id, key in enumerate(blocks["keys"]):
            identity = identities.get(key)
            expected = state_ids[identity["default_state"]] if identity is not None else None
            require_equal(defaults[block_id], expected, f"{version}/{key} default state")
            if identity is not None:
                memberships[key].update(identity["states"])
    covered = set()
    for block_id, key in enumerate(blocks["keys"]):
        span = blocks["state_spans"][block_id]
        if (not isinstance(span, list) or len(span) != 2
                or any(type(value) is not int for value in span)
                or span[0] < 0 or span[1] <= 0 or sum(span) > len(states["keys"])):
            raise ValueError(f"identity bundle: {key} invalid state span")
        start, count = span
        keys = states["keys"][start:start + count]
        if len(keys) != len(memberships[key]) or set(keys) != memberships[key]:
            raise ValueError(f"identity bundle: {key} semantic state span differs")
        for state_id in range(start, start + count):
            if state_id in covered:
                raise ValueError(f"identity bundle: {key} overlapping state span")
            covered.add(state_id)
            require_equal(states["block_ids"][state_id], block_id, f"{states['keys'][state_id]} owner")
    require_equal(len(covered), len(states["keys"]), "state span coverage")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-reports", type=Path, default=census.ROOT / ".cache/mc/26.2/generated/reports")
    parser.add_argument("--latest-reports", type=Path, default=census.ROOT / ".cache/mc/26.3/generated/reports")
    parser.add_argument("--census", type=Path, help="validate and consume an existing census manifest")
    parser.add_argument("--scope", choices=SCOPES, default="union")
    destination = parser.add_mutually_exclusive_group()
    destination.add_argument("--output", type=Path, help="create a staging bundle; refuses to overwrite")
    destination.add_argument("--check", type=Path, help="validate an existing staging bundle byte for byte")
    args = parser.parse_args()
    sources = {
        census.VERSIONS[0]: census.load_source(args.base_reports),
        census.VERSIONS[1]: census.load_source(args.latest_reports),
    }
    manifest = census.read_report(args.census)[0] if args.census else census.build_manifest(sources)
    bundle = build_bundle(manifest, sources, args.scope)
    encoded = census.serialize(bundle)
    if args.check:
        existing, _ = census.read_report(args.check)
        validate_bundle(existing, manifest, sources)
        if args.check.read_text(encoding="utf-8") != encoded:
            raise SystemExit(f"identity bundle differs: {args.check}")
    if args.output:
        with args.output.open("x", encoding="utf-8") as output:
            output.write(encoded)
    for name in census.DOMAINS:
        print(f"{name}: {len(bundle['domains'][name]['keys'])}")
    print(f"scope: {args.scope}; bundle_sha256: {digest(bundle)}")


if __name__ == "__main__":
    main()
