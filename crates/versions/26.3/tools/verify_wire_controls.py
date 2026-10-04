#!/usr/bin/env python3
import argparse
import hashlib
import json
from pathlib import Path
import struct
import zipfile


def varint(value):
    if not -(1 << 31) <= value < (1 << 31):
        raise ValueError('value is outside signed 32-bit range')
    value &= 0xffffffff
    encoded = bytearray()
    while value > 127:
        encoded.append((value & 127) | 128)
        value >>= 7
    encoded.append(value)
    return bytes(encoded)


def identifier(value):
    encoded = value.encode('utf8')
    return varint(len(encoded)) + encoded


def body(name, case):
    if name.startswith('post_effects_'):
        return varint(len(case['effects'])) + b''.join(map(identifier, case['effects']))
    if name == 'transient_block':
        x, y, z = case['pos']
        packed = ((x & 0x3ffffff) << 38) | ((z & 0x3ffffff) << 12) | (y & 0xfff)
        return struct.pack('>Q', packed) + varint(case['wire_state'])
    if name in ('swing', 'signed_swing'):
        return b''.join(varint(case[key]) for key in ('entity_id', 'hand', 'kind', 'duration_ticks'))
    raise ValueError(f'unknown fixture {name}')


def require_equal(name, actual, expected):
    if actual != expected:
        raise ValueError(f'{name}: expected {expected!r}, got {actual!r}')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--fixtures', type=Path, default=Path(__file__).resolve().parent.parent /
                        'tests/fixtures/new_packet_wire_controls_26_3.json')
    parser.add_argument('--jar', type=Path)
    parser.add_argument('--packet-report', type=Path)
    args = parser.parse_args()
    fixture = json.loads(args.fixtures.read_text())
    require_equal('fixture format', fixture['format_version'], 1)
    require_equal('protocol', fixture['protocol'], 777)
    require_equal('fixture cases', len(fixture['cases']), 5)
    for name, case in fixture['cases'].items():
        require_equal(name, body(name, case).hex(), case['body_hex'])
    swing = fixture['cases']['swing']
    swapped = b''.join(varint(swing[key]) for key in ('entity_id', 'hand', 'duration_ticks', 'kind'))
    for name, wrong in [('swapped swing fields', swapped), ('extra trailing byte', body('swing', swing) + b'\x00')]:
        try:
            require_equal(name, wrong.hex(), swing['body_hex'])
        except ValueError:
            print(f'negative control rejected: {name}')
        else:
            raise ValueError(f'negative control did not fail: {name}')
    if args.jar:
        require_equal('jar digest', hashlib.sha256(args.jar.read_bytes()).hexdigest(), fixture['server_jar_sha256'])
        wanted = set(fixture['class_sha256'].values())
        with zipfile.ZipFile(args.jar) as jar:
            for entry in jar.infolist():
                if entry.filename.endswith('.class'):
                    wanted.discard(hashlib.sha256(jar.read(entry)).hexdigest())
        if wanted:
            raise ValueError(f'{len(wanted)} reviewed class digests are absent')
        print(f'authenticated {len(fixture["class_sha256"])} reviewed class digests')
    if args.packet_report:
        raw = args.packet_report.read_bytes()
        require_equal('packet report digest', hashlib.sha256(raw).hexdigest(), fixture['packet_report_sha256'])
        report = json.loads(raw)
        for key, expected in fixture['packet_ids'].items():
            state, direction, name = key.split('/', 2)
            require_equal(key, report[state][direction][name]['protocol_id'], expected)
        print(f'authenticated {len(fixture["packet_ids"])} selected packet identifiers')
    print(f'verified {len(fixture["cases"])} independent arithmetic wire bodies')


if __name__ == '__main__':
    main()
