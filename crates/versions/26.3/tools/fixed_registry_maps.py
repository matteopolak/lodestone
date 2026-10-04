#!/usr/bin/env python3
"""Generate release-scoped fixed-registry identity maps from official reports."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys
import zipfile


ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(ROOT / "crates/lodestone-data/tools"))
import canonical_census as census
import identity_staging as identities

from class_contract import Class


OUTPUT = ROOT / "crates/versions/26.3/src/generated/fixed_registries.rs"
VERSIONS = census.VERSIONS
IDENTITY_DOMAINS = {"minecraft:block": "blocks", "minecraft:item": "items"}
PARTICLE_BOOTSTRAP = "net/minecraft/core/particles/ParticleTypes.class"
DATA_TABLES = {
    "minecraft:data_component_type": ("data_component_types.rs", "DATA_COMPONENT_TYPE"),
    "minecraft:particle_type": ("particle_types.rs", "PARTICLE_TYPE"),
}


def registry_names(report, registry, version):
    if registry not in report:
        return []
    entries = report[registry]["entries"]
    for name in entries:
        if not isinstance(name, str) or not re.fullmatch(r"[a-z0-9_.-]+:[a-z0-9_./-]+", name):
            raise ValueError(f"{registry}/{version}: invalid resource name: {name!r}")
    return census.ordered_keys(
        ((name, entry["protocol_id"]) for name, entry in entries.items()),
        f"{registry}/{version}",
    )


def retained_union(base, latest):
    seen = set(base)
    return base + [name for name in latest if name not in seen]


def build_manifest(reports, report_sha256, identity_bundle=None):
    domains = {}
    for registry in sorted(set().union(*(reports[version] for version in VERSIONS))):
        wire_names = {
            version: registry_names(reports[version], registry, version) for version in VERSIONS
        }
        base, latest = (wire_names[version] for version in VERSIONS)
        if registry in IDENTITY_DOMAINS and identity_bundle is not None:
            keys = identity_bundle["domains"][IDENTITY_DOMAINS[registry]]["keys"][:]
        elif registry == "minecraft:sound_event":
            keys = census.registry_union_names(reports, registry)
        else:
            keys = retained_union(base, latest)
        canonical = {name: index for index, name in enumerate(keys)}
        versions = {}
        for version in VERSIONS:
            wire = {name: index for index, name in enumerate(wire_names[version])}
            versions[version] = {
                "wire_count": len(wire),
                "wire_to_canonical": [canonical[name] for name in wire_names[version]],
                "canonical_to_wire": [wire.get(name) for name in keys],
            }
        domains[registry] = {"keys": keys, "versions": versions}
    manifest = {"schema_version": 1, "source_sha256": dict(report_sha256), "domains": domains}
    validate_manifest(manifest, reports, report_sha256, identity_bundle)
    return manifest


def validate_manifest(manifest, reports, report_sha256, identity_bundle=None):
    if manifest["schema_version"] != 1:
        raise ValueError("fixed registry manifest: unsupported schema")
    if manifest["source_sha256"] != report_sha256:
        raise ValueError("fixed registry manifest: report provenance differs")
    expected_domains = set().union(*(reports[version] for version in VERSIONS))
    if set(manifest["domains"]) != expected_domains:
        raise ValueError("fixed registry manifest: registry coverage differs")
    for registry in sorted(expected_domains):
        domain = manifest["domains"][registry]
        keys = domain["keys"]
        sources = {version: registry_names(reports[version], registry, version) for version in VERSIONS}
        base, latest = (sources[version] for version in VERSIONS)
        if keys[:len(base)] != base:
            raise ValueError(f"{registry}: canonical base prefix changed")
        expected_keys = retained_union(base, latest)
        if keys != expected_keys or len(set(keys)) != len(keys):
            raise ValueError(f"{registry}: canonical additions or retained removals differ")
        if len(keys) > 65535:
            raise ValueError(f"{registry}: canonical IDs exceed u16 representation")
        if set(domain["versions"]) != set(VERSIONS):
            raise ValueError(f"{registry}: release coverage differs")
        canonical = {name: index for index, name in enumerate(keys)}
        for version in VERSIONS:
            names = sources[version]
            wire = {name: index for index, name in enumerate(names)}
            mapping = domain["versions"][version]
            ingress = mapping["wire_to_canonical"]
            egress = mapping["canonical_to_wire"]
            if (type(mapping["wire_count"]) is not int or mapping["wire_count"] != len(names)
                    or len(ingress) != len(names) or len(egress) != len(keys)):
                raise ValueError(f"{registry}/{version}: mapping length differs")
            for raw, name in enumerate(names):
                value = ingress[raw]
                if type(value) is not int or value != canonical[name]:
                    raise ValueError(f"{registry}/{version}: wrong ingress at wire ID {raw} ({name})")
            for raw, name in enumerate(keys):
                value = egress[raw]
                if value != wire.get(name) or (value is not None and type(value) is not int):
                    raise ValueError(f"{registry}/{version}: wrong egress at canonical ID {raw} ({name})")
        if registry in IDENTITY_DOMAINS and identity_bundle is not None:
            identity = identity_bundle["domains"][IDENTITY_DOMAINS[registry]]
            if keys != identity["keys"]:
                raise ValueError(f"{registry}: canonical identity bundle differs")
            for version in VERSIONS:
                for field in ("wire_to_canonical", "canonical_to_wire"):
                    if domain["versions"][version][field] != identity["versions"][version][field]:
                        raise ValueError(f"{registry}/{version}: identity bundle {field} differs")
        if registry == "minecraft:sound_event" and keys != census.registry_union_names(reports, registry):
            raise ValueError("sound events: canonical data policy differs")


def compact_runs(values):
    rows = []
    for source, target in enumerate(values):
        if target is None:
            continue
        if (rows and source == rows[-1][0] + rows[-1][2]
                and target == rows[-1][1] + rows[-1][2]):
            rows[-1][2] += 1
        else:
            rows.append([source, target, 1])
    return rows


def translate_runs(rows, source):
    for start, target, length in rows:
        if start <= source < start + length:
            return target + source - start
    return None


def registry_variant(registry):
    if not re.fullmatch(r"minecraft:[a-z0-9_/]+", registry):
        raise ValueError(f"unsupported registry identifier: {registry}")
    return "".join(word.capitalize() for word in re.split(r"[_/]", registry.split(":", 1)[1]))


def run_literal(values):
    rows = compact_runs(values)
    return "&[" + ", ".join(f"Run::new({source}, {target}, {length})" for source, target, length in rows) + "]"


def render(manifest):
    domains = manifest["domains"]
    variants = [registry_variant(registry) for registry in domains]
    if len(set(variants)) != len(variants) or len(variants) > 256:
        raise ValueError("registry enum variants collide or exceed u8 representation")
    lines = [
        "// Generated by tools/fixed_registry_maps.py from official registry reports.",
        "// Names identify values; packet and component body policies remain independent.",
    ]
    lines.extend(f"// {version} registries.json SHA256: {manifest['source_sha256'][version]}" for version in VERSIONS)
    lines.extend([
        "", "#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]",
        "#[repr(u8)]", "pub enum FixedRegistry {",
    ])
    lines.extend(f"    {variant}," for variant in variants)
    lines.extend(["}", "", "impl FixedRegistry {", f"    pub const ALL: [Self; {len(variants)}] = ["])
    lines.extend(f"        Self::{variant}," for variant in variants)
    lines.extend(["    ];", "}", ""])
    for index, (registry, domain) in enumerate(domains.items()):
        keys = domain["keys"]
        lines.extend([f"static NAMES_{index}: [&str; {len(keys)}] = [", *[f"    {json.dumps(name)}," for name in keys], "];", ""])
        by_name = sorted(range(len(keys)), key=keys.__getitem__)
        lines.append(f"static NAME_ORDER_{index}: [u16; {len(keys)}] = [")
        lines.extend("    " + ", ".join(map(str, by_name[start:start + 16])) + "," for start in range(0, len(by_name), 16))
        lines.extend(["];", ""])
    lines.append(f"pub(super) static TABLES: [RegistryTable; {len(domains)}] = [")
    for index, (registry, domain) in enumerate(domains.items()):
        lines.extend([
            "    RegistryTable {", f"        name: {json.dumps(registry)},",
            f"        canonical_names: &NAMES_{index},", f"        name_order: &NAME_ORDER_{index},",
            "        versions: [",
        ])
        for version in VERSIONS:
            mapping = domain["versions"][version]
            lines.extend([
                "            VersionTable {", f"                wire_count: {mapping['wire_count']},",
                f"                from_wire: {run_literal(mapping['wire_to_canonical'])},",
                f"                to_wire: {run_literal(mapping['canonical_to_wire'])},",
                "            },",
            ])
        lines.extend(["        ],", "    },"])
    return "\n".join([*lines, "];", ""])


def load_inputs(base_reports, latest_reports):
    directories = dict(zip(VERSIONS, (base_reports, latest_reports)))
    sources = {version: census.load_source(directory) for version, directory in directories.items()}
    canonical = census.build_manifest(sources)
    bundle = identities.build_bundle(canonical, sources)
    reports, hashes = {}, {}
    for version, directory in directories.items():
        reports[version], hashes[version] = census.read_report(directory / "registries.json")
    return reports, hashes, bundle


def particle_registrations(instructions, expected_names):
    flags = {}
    pending = None
    owner = PARTICLE_BOOTSTRAP.removesuffix(".class")
    simple = "(Ljava/lang/String;Z)"
    complex_parameters = "(Ljava/lang/String;ZLjava/util/function/Function;Ljava/util/function/Function;)"
    for _, opcode, value in instructions:
        if opcode in ("ldc", "ldc_w") and isinstance(value, str):
            pending = "minecraft:" + value
        if opcode != "invokestatic" or not isinstance(value, tuple) or value[0] != owner:
            continue
        parameters = value[1][1].partition(")")[0] + ")"
        if parameters not in (simple, complex_parameters):
            continue
        if pending is None or pending in flags:
            raise ValueError("particle bootstrap: absent or duplicate registration name")
        flags[pending] = parameters == simple
        pending = None
    if set(flags) != set(expected_names):
        raise ValueError("particle bootstrap: report name coverage differs")
    return flags


def load_particle_inputs(jars, reports):
    flags, digests = {}, {}
    for version in VERSIONS:
        with zipfile.ZipFile(jars[version]) as jar:
            raw = jar.read(PARTICLE_BOOTSTRAP)
        parsed = Class(raw)
        instructions = parsed.instructions(parsed.methods[("<clinit>", "()V")])
        names = registry_names(reports[version], "minecraft:particle_type", version)
        flags[version] = particle_registrations(instructions, names)
        digests[version] = hashlib.sha256(raw).hexdigest()
    return flags, digests


def canonical_particle_flags(manifest, flags):
    domain = manifest["domains"]["minecraft:particle_type"]
    for version in VERSIONS:
        names = {name for name, raw in zip(domain["keys"], domain["versions"][version]["canonical_to_wire"]) if raw is not None}
        if set(flags[version]) != names or any(type(value) is not bool for value in flags[version].values()):
            raise ValueError(f"particle bootstrap/{version}: invalid or incomplete flags")
    result = []
    for name in domain["keys"]:
        observed = [flags[version][name] for version in VERSIONS if name in flags[version]]
        if len(set(observed)) != 1:
            raise ValueError(f"particle bootstrap: {name} changed payload classification")
        result.append(observed[0])
    return result


def render_data_tables(manifest, particle_flags, particle_sha256):
    files = {}
    for registry, (filename, constant) in DATA_TABLES.items():
        names = manifest["domains"][registry]["keys"]
        lines = [
            "// Generated by crates/versions/26.3/tools/fixed_registry_maps.py --emit-data-tables.",
            "// Canonical names retain the 26.2 prefix and append 26.3 identities.",
        ]
        lines.extend(f"// {version} registries.json SHA256: {manifest['source_sha256'][version]}" for version in VERSIONS)
        lines.extend([
            "", f"pub const {constant}_COUNT: u32 = {len(names)};", "",
            f"pub static {constant}_NAMES: [&str; {len(names)}] = [",
            *[f"    {json.dumps(name)}," for name in names], "];", "",
        ])
        if registry == "minecraft:particle_type":
            values = canonical_particle_flags(manifest, particle_flags)
            lines.extend(f"// {version} particle bootstrap class SHA256: {particle_sha256[version]}" for version in VERSIONS)
            lines.extend([f"pub static PARTICLE_TYPE_IS_SIMPLE: [bool; {len(values)}] = ["])
            lines.extend(f"    {str(value).lower()}," for value in values)
            lines.extend(["];", ""])
        files[filename] = "\n".join(lines)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-reports", type=Path, default=ROOT / ".cache/mc/26.2/generated/reports")
    parser.add_argument("--latest-reports", type=Path, default=ROOT / ".cache/mc/26.3/generated/reports")
    parser.add_argument("--output", type=Path, default=OUTPUT)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--emit-data-tables", action="store_true")
    parser.add_argument("--base-jar", type=Path, default=ROOT / ".cache/mc/26.2/versions/26.2/server-26.2.jar")
    parser.add_argument("--latest-jar", type=Path, default=ROOT / ".cache/mc/26.3/versions/26.3/server-26.3.jar")
    args = parser.parse_args()
    inputs = load_inputs(args.base_reports, args.latest_reports)
    manifest = build_manifest(*inputs)
    text = render(manifest)
    if args.check:
        if not args.output.is_file() or args.output.read_text(encoding="utf-8") != text:
            raise SystemExit(f"generated fixed registry table differs: {args.output}")
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    print(f"{args.output}: {'verified' if args.check else 'generated'}")
    print(f"{len(manifest['domains'])} fixed registries; {sum(len(domain['keys']) for domain in manifest['domains'].values())} canonical identities; {len(text.encode())} source bytes")
    print(f"source_sha256: {hashlib.sha256(text.encode()).hexdigest()}")
    if args.emit_data_tables:
        flags, digests = load_particle_inputs(dict(zip(VERSIONS, (args.base_jar, args.latest_jar))), inputs[0])
        files = render_data_tables(manifest, flags, digests)
        for filename, contents in files.items():
            path = ROOT / "crates/lodestone-data/src/generated" / filename
            if args.check:
                if path.read_text(encoding="utf-8") != contents:
                    raise SystemExit(f"generated canonical data table differs: {path}")
            else:
                path.write_text(contents, encoding="utf-8")
            print(f"{path}: {'verified' if args.check else 'generated'}")


if __name__ == "__main__":
    main()
