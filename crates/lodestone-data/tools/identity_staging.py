#!/usr/bin/env python3
"""Stage canonical identity columns without compiling or changing runtime tables."""

import argparse
import copy
import hashlib
import json
from pathlib import Path
import re

import canonical_census as census


SCOPES = ("base", "union")
RUNTIME_BLOCK_FILES = ("block_registry.rs", "block_enum.rs", "block_states.rs")
RUNTIME_UNION_FILES = (*RUNTIME_BLOCK_FILES, "items.rs", "item_enum.rs", "identity_versions.rs")


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


def enum_variants(keys):
    variants = []
    seen = {}
    for key in keys:
        namespace, separator, path = key.partition(":")
        if namespace != "minecraft" or not separator or not re.fullmatch(r"[a-z_][a-z0-9_]*", path):
            raise ValueError(f"Rust identities: unsupported built-in name: {key}")
        variant = "".join(word[0].upper() + word[1:] for word in path.split("_") if word)
        if not re.fullmatch(r"[A-Z][A-Za-z0-9]*", variant) or variant == "Self":
            raise ValueError(f"Rust identities: invalid enum variant for {key}")
        if variant in seen:
            raise ValueError(f"Rust identities: enum variant collision: {seen[variant]} and {key}")
        seen[variant] = key
        variants.append(variant)
    return variants


def state_properties(key):
    name, separator, suffix = key.partition("[")
    if not separator:
        return ()
    if not suffix.endswith("]"):
        raise ValueError(f"Rust identities: invalid state properties: {key}")
    pairs = []
    for pair in suffix[:-1].split(","):
        property_name, equals, value = pair.partition("=")
        if not equals or not all(re.fullmatch(r"[a-z0-9_]+", part) for part in (property_name, value)):
            raise ValueError(f"Rust identities: unsupported state properties: {key}")
        pairs.append((property_name, value))
    if census.state_key(name, dict(pairs)) != key:
        raise ValueError(f"Rust identities: noncanonical state properties: {key}")
    return tuple(pairs)


def require_width(count, maximum, label):
    if count > maximum:
        raise ValueError(f"Rust identities: {label} count {count} exceeds representation limit {maximum}")


def rust_array(name, element_type, values, width=16):
    lines = [f"pub static {name}: [{element_type}; {len(values)}] = ["]
    lines.extend("    " + ", ".join(values[start:start + width]) + ","
                 for start in range(0, len(values), width))
    return "\n".join([*lines, "];", ""])


def rust_enum(name, variants, keys):
    lines = ["#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]",
             "#[repr(u16)]", f"pub enum {name} {{"]
    for identity_id, (variant, key) in enumerate(zip(variants, keys)):
        lines.extend([f"    /// `{key}`", f"    {variant} = {identity_id},"])
    lines.extend(["}", ""])
    by_id = rust_array(f"{name.upper()}S_BY_REGISTRY_ID", name,
                       [f"{name}::{variant}" for variant in variants], 4)
    by_name = rust_array("REGISTRY_IDS_BY_NAME", "u16",
                         [str(index) for index in sorted(range(len(keys)), key=keys.__getitem__)])
    return "\n".join([*lines, by_id, by_name])


def canonical_default_states(bundle, sources):
    state_ids = {key: index for index, key in enumerate(bundle["domains"]["block_states"]["keys"])}
    defaults = []
    for key in bundle["domains"]["blocks"]["keys"]:
        identities = [sources[version]["block_identities"].get(key) for version in census.VERSIONS]
        marked = [identity["default_state"] for identity in identities if identity is not None]
        if len(set(marked)) != 1:
            raise ValueError(f"Rust identities: {key} has conflicting semantic defaults; explicit policy required")
        defaults.append(state_ids[marked[0]])
    return defaults


def build_rust_files(bundle, manifest, sources):
    """Render validated identity columns; no behavior data or runtime activation."""
    validate_bundle(bundle, manifest, sources)
    blocks = bundle["domains"]["blocks"]
    states = bundle["domains"]["block_states"]
    items = bundle["domains"]["items"]
    canonical_defaults = canonical_default_states(bundle, sources)
    require_width(len(blocks["keys"]), 65535, "blocks (u16 count)")
    require_width(len(items["keys"]), 65535, "items (u16 count)")
    require_width(len(states["keys"]), 4294967295, "states (u32 count)")
    block_variants = enum_variants(blocks["keys"])
    item_variants = enum_variants(items["keys"])
    properties = [state_properties(key) for key in states["keys"]]
    property_sets = sorted(set(properties))
    require_width(len(property_sets), 65536, "property sets (u16 index)")
    property_ids = {value: index for index, value in enumerate(property_sets)}
    alphabetical_ids = sorted(range(len(blocks["keys"])), key=blocks["keys"].__getitem__)
    alphabetical_index = {block_id: index for index, block_id in enumerate(alphabetical_ids)}
    state_rows = [f"({alphabetical_index[owner]}, {property_ids[prop]})"
                  for owner, prop in zip(states["block_ids"], properties)]
    header = ("// @generated by crates/lodestone-data/tools/identity_staging.py.\n"
              f"// Scope: {bundle['scope']}; bundle_sha256: {digest(bundle)}.\n"
              "// Canonical identity data; wire IDs require explicit per-version translation.\n\n")
    files = {
        "block_registry.rs": "\n".join([
            f"pub const BLOCK_COUNT: u32 = {len(blocks['keys'])};\n",
            rust_array("BLOCK_REGISTRY_NAMES", "&str", [json.dumps(value) for value in blocks["keys"]], 1),
            rust_array("STATE_BLOCK", "u16", [str(value) for value in states["block_ids"]]),
            rust_array("BLOCK_STATE_SPANS", "(u32, u32)",
                       [f"({start}, {count})" for start, count in blocks["state_spans"]], 8),
        ]),
        "block_enum.rs": rust_enum("Block", block_variants, blocks["keys"]) + "\n" +
                         rust_array("DEFAULT_STATE", "u32", [str(value) for value in canonical_defaults]),
        "block_states.rs": "\n".join([
            f"pub const STATE_COUNT: u32 = {len(states['keys'])};\n",
            rust_array("PROPERTY_SETS", "&[(&str, &str)]",
                       ["&[" + ", ".join(f"({json.dumps(key)}, {json.dumps(value)})" for key, value in pairs) + "]"
                        for pairs in property_sets], 1),
            rust_array("STATES", "(u16, u16)", state_rows, 12),
        ]),
        "items.rs": "\n".join([
            f"pub const ITEM_COUNT: u32 = {len(items['keys'])};\n",
            rust_array("ITEM_NAMES", "&str", [json.dumps(value) for value in items["keys"]], 1),
        ]),
        "item_enum.rs": rust_enum("Item", item_variants, items["keys"]),
    }
    version_lines = []
    for version in selected_versions(bundle["scope"]):
        version_lines.append(f"pub mod v{version.replace('.', '_')} {{")
        defaults = blocks["versions"][version]["default_states"]
        columns = [rust_array("BLOCK_DEFAULT_STATES", "Option<u32>",
                              ["None" if value is None else f"Some({value})" for value in defaults], 8)]
        for name in census.DOMAINS:
            mapping = bundle["domains"][name]["versions"][version]
            require_width(len(mapping["wire_to_canonical"]), 4294967295, f"{name}/{version} wire (u32 count)")
            prefix = {"blocks": "BLOCK", "block_states": "BLOCK_STATE", "items": "ITEM"}[name]
            columns.extend([
                f"pub const WIRE_{prefix}_COUNT: u32 = {len(mapping['wire_to_canonical'])};\n",
                rust_array(f"{prefix}_WIRE_TO_CANONICAL", "u32",
                           [str(value) for value in mapping["wire_to_canonical"]]),
                rust_array(f"{prefix}_CANONICAL_TO_WIRE", "Option<u32>",
                           ["None" if value is None else f"Some({value})" for value in mapping["canonical_to_wire"]], 8),
            ])
        version_lines.extend("    " + line if line else "" for line in "\n".join(columns).splitlines())
        version_lines.extend(["}", ""])
    files["identity_versions.rs"] = "\n".join(version_lines)
    files = {name: header + content for name, content in files.items()}
    files["manifest.json"] = census.serialize({
        "schema_version": 1,
        "scope": bundle["scope"],
        "bundle_sha256": digest(bundle),
        "census_sha256": bundle["census_sha256"],
        "source_sha256": bundle["source_sha256"],
        "counts": {name: len(bundle["domains"][name]["keys"]) for name in census.DOMAINS},
        "file_sha256": {name: hashlib.sha256(content.encode()).hexdigest() for name, content in files.items()},
    })
    return files


def write_rust_files(directory, files):
    directory = directory.resolve()
    roots = ((census.ROOT / ".cache").resolve(), Path("/tmp").resolve(), Path("/private/tmp").resolve())
    if not any(root in directory.parents for root in roots):
        raise ValueError("Rust identities: output must be a private directory under .cache or /tmp")
    directory.mkdir(parents=True, exist_ok=False)
    for name, content in files.items():
        with (directory / name).open("x", encoding="utf-8") as output:
            output.write(content)


def check_rust_files(directory, files):
    actual_names = sorted(path.name for path in directory.iterdir())
    if actual_names != sorted(files):
        raise ValueError(f"Rust identities: staged file set differs: {directory}")
    for name, content in files.items():
        if (directory / name).read_bytes() != content.encode():
            raise ValueError(f"Rust identities: staged file differs: {directory / name}")


def check_runtime_files(directory, files, scope):
    selected_versions(scope)
    for name in RUNTIME_BLOCK_FILES if scope == "base" else RUNTIME_UNION_FILES:
        if (directory / name).read_bytes() != files[name].encode():
            raise ValueError(f"Rust identities: runtime block file differs: {directory / name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-reports", type=Path, default=census.ROOT / ".cache/mc/26.2/generated/reports")
    parser.add_argument("--latest-reports", type=Path, default=census.ROOT / ".cache/mc/26.3/generated/reports")
    parser.add_argument("--census", type=Path, help="validate and consume an existing census manifest")
    parser.add_argument("--bundle", type=Path, help="validate and consume an existing identity bundle")
    parser.add_argument("--scope", choices=SCOPES, default="union")
    destination = parser.add_mutually_exclusive_group()
    destination.add_argument("--output", type=Path, help="create a staging bundle; refuses to overwrite")
    destination.add_argument("--check", type=Path, help="validate an existing staging bundle byte for byte")
    destination.add_argument("--rust-output", type=Path, help="create a private Rust staging directory; refuses to overwrite")
    destination.add_argument("--rust-check", type=Path, help="check a Rust staging directory byte for byte")
    destination.add_argument("--runtime-check", type=Path, nargs="?",
                             const=census.ROOT / "crates/lodestone-data/src/generated",
                             help="check the adopted block files for base scope or all six union identity files")
    args = parser.parse_args()
    sources = {
        census.VERSIONS[0]: census.load_source(args.base_reports),
        census.VERSIONS[1]: census.load_source(args.latest_reports),
    }
    manifest = census.read_report(args.census)[0] if args.census else census.build_manifest(sources)
    bundle = census.read_report(args.bundle)[0] if args.bundle else build_bundle(manifest, sources, args.scope)
    validate_bundle(bundle, manifest, sources)
    require_equal(bundle["scope"], args.scope, "requested scope")
    encoded = census.serialize(bundle)
    if args.check:
        existing, _ = census.read_report(args.check)
        validate_bundle(existing, manifest, sources)
        if args.check.read_text(encoding="utf-8") != encoded:
            raise SystemExit(f"identity bundle differs: {args.check}")
    if args.output:
        with args.output.open("x", encoding="utf-8") as output:
            output.write(encoded)
    if args.rust_output or args.rust_check or args.runtime_check:
        files = build_rust_files(bundle, manifest, sources)
        if args.rust_output:
            write_rust_files(args.rust_output, files)
        elif args.rust_check:
            check_rust_files(args.rust_check, files)
        else:
            check_runtime_files(args.runtime_check, files, args.scope)
        print(f"Rust files: {len(files) - 1}; manifest_sha256: {hashlib.sha256(files['manifest.json'].encode()).hexdigest()}")
    for name in census.DOMAINS:
        print(f"{name}: {len(bundle['domains'][name]['keys'])}")
    print(f"scope: {args.scope}; bundle_sha256: {digest(bundle)}")


if __name__ == "__main__":
    main()
