#!/usr/bin/env python3
"""Bounded raw-wire capture of a local official 26.3 server connection.

Run ``python3 capture_connection.py --record`` to refresh the compact witness,
or omit ``--record`` to compare a fresh connection with the committed witness.
The full decompressed packet bodies can be saved with ``--raw-output PATH``.
This tool deliberately does not call the Lodestone adapter.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid
import zlib


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
JAR = ROOT / ".cache/mc/26.3/server.jar"
FIXTURE = HERE / "fixtures/connection_26_3.json"
JAR_SHA1 = "33680f5f2ac32864d6d7cf5e56a705fdb3e05f4c"
PORT = 25677
PROTOCOL = 777
MAX_FRAME = 8 * 1024 * 1024
MAX_CAPTURE = 32 * 1024 * 1024
MAX_ORACLE_DISK = 512 * 1024 * 1024
DEADLINE_SECONDS = 100
RAW_UNSTABLE_REGISTRIES = {
    "minecraft:worldgen/biome", "minecraft:dimension_type", "minecraft:timeline",
}

IDS = {
    "login": {"compression": 3, "finished": 2, "disconnect": 0},
    "configuration": {
        "finish": 3, "keep_alive": 4, "ping": 5, "registry": 7,
        "tags": 14, "known_packs": 15, "disconnect": 2,
    },
    "play": {"login": 50, "keep_alive": 45, "disconnect": 32},
}


def varint(value):
    value &= 0xFFFFFFFF
    out = bytearray()
    while value > 0x7F:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value)
    return bytes(out)


def string(value):
    data = value.encode("utf-8")
    return varint(len(data)) + data


class Cursor:
    def __init__(self, data):
        self.data = data
        self.at = 0

    def take(self, count):
        if count < 0 or self.at + count > len(self.data):
            raise ValueError("short packet body")
        result = self.data[self.at:self.at + count]
        self.at += count
        return result

    def number(self):
        value = 0
        for shift in range(0, 35, 7):
            byte = self.take(1)[0]
            value |= (byte & 0x7F) << shift
            if byte < 0x80:
                return value
        raise ValueError("VarInt exceeds five bytes")

    def text(self):
        size = self.number()
        if size > 32767 * 4:
            raise ValueError("oversized string")
        return self.take(size).decode("utf-8")

    def done(self):
        if self.at != len(self.data):
            raise ValueError(f"{len(self.data) - self.at} trailing bytes")


def read_nbt(cursor, kind, depth=0):
    if depth > 32:
        raise ValueError("NBT nesting limit exceeded")
    if kind == 0:
        return None
    if kind in (1, 2, 3, 4):
        return int.from_bytes(cursor.take({1: 1, 2: 2, 3: 4, 4: 8}[kind]), "big", signed=True)
    if kind in (5, 6):
        return cursor.take(4 if kind == 5 else 8).hex()
    if kind == 7:
        return cursor.take(checked_count(cursor, 1)).hex()
    if kind == 8:
        return cursor.take(int.from_bytes(cursor.take(2), "big")).decode("utf-8")
    if kind == 9:
        item_kind = cursor.take(1)[0]
        return [read_nbt(cursor, item_kind, depth + 1) for _ in range(checked_count(cursor, 0))]
    if kind == 10:
        result = {}
        while True:
            item_kind = cursor.take(1)[0]
            if item_kind == 0:
                break
            key = cursor.take(int.from_bytes(cursor.take(2), "big")).decode("utf-8")
            if key in result:
                raise ValueError("duplicate NBT compound key")
            result[key] = read_nbt(cursor, item_kind, depth + 1)
        return result
    if kind in (11, 12):
        width = 4 if kind == 11 else 8
        body = cursor.take(checked_count(cursor, width))
        return [int.from_bytes(body[i:i + width], "big", signed=True) for i in range(0, len(body), width)]
    raise ValueError(f"unknown NBT kind {kind}")


def checked_count(cursor, width):
    count = int.from_bytes(cursor.take(4), "big", signed=True)
    if not 0 <= count <= MAX_FRAME:
        raise ValueError(f"invalid NBT count {count}")
    if width and count * width > MAX_FRAME:
        raise ValueError("NBT array exceeds packet cap")
    return count * width if width else count


def registry_names(body):
    cursor = Cursor(body)
    name = cursor.text()
    count = cursor.number()
    if count > 65536:
        raise ValueError("registry entry count exceeds cap")
    entries = []
    content = []
    for _ in range(count):
        entry_name = cursor.text()
        entries.append(entry_name)
        present = cursor.take(1)[0]
        data = read_nbt(cursor, cursor.take(1)[0]) if present else None
        content.append([entry_name, data])
    cursor.done()
    return name, entries, stable_digest(content)


def stable_digest(value):
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def tag_order(body):
    cursor = Cursor(body)
    registry_count = cursor.number()
    if registry_count > 256:
        raise ValueError("tag registry count exceeds cap")
    registries = []
    for _ in range(registry_count):
        name = cursor.text()
        tag_count = cursor.number()
        if tag_count > 65536:
            raise ValueError("tag count exceeds cap")
        names = []
        membership = {}
        for _ in range(tag_count):
            tag_name = cursor.text()
            names.append(tag_name)
            members = cursor.number()
            if members > 65536:
                raise ValueError("tag member count exceeds cap")
            ids = []
            for _ in range(members):
                ids.append(cursor.number())
            membership[tag_name] = sorted(ids)
        registries.append({"name": name, "tags": names, "membership_sha256": stable_digest(membership)})
    cursor.done()
    return registries


def recv_exact(sock, size):
    out = bytearray()
    while len(out) < size:
        part = sock.recv(size - len(out))
        if not part:
            raise EOFError("server closed the connection")
        out.extend(part)
    return bytes(out)


def recv_varint(sock):
    value = 0
    for shift in range(0, 35, 7):
        byte = recv_exact(sock, 1)[0]
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value
    raise ValueError("frame VarInt exceeds five bytes")


def read_packet(sock, compression):
    size = recv_varint(sock)
    if not 0 < size <= MAX_FRAME:
        raise ValueError(f"invalid frame length {size}")
    frame = recv_exact(sock, size)
    if compression is not None:
        cursor = Cursor(frame)
        expanded = cursor.number()
        content = frame[cursor.at:]
        if expanded:
            if not 0 < expanded <= MAX_FRAME:
                raise ValueError("decompressed frame exceeds cap")
            inflater = zlib.decompressobj()
            content = inflater.decompress(content, expanded + 1)
            if len(content) != expanded or not inflater.eof or inflater.unused_data:
                raise ValueError("invalid compressed frame")
        frame = content
    cursor = Cursor(frame)
    packet_id = cursor.number()
    return packet_id, frame[cursor.at:]


def send_packet(sock, packet_id, body=b"", compression=None):
    payload = varint(packet_id) + body
    if compression is not None:
        payload = varint(len(payload)) + zlib.compress(payload) if len(payload) >= compression else b"\0" + payload
    sock.sendall(varint(len(payload)) + payload)


def client_information():
    return string("en_us") + bytes((8, 0, 1, 0, 1, 0, 0, 0))


def witness(packet_id, body):
    return {"id": packet_id, "length": len(body), "sha256": hashlib.sha256(body).hexdigest()}


def check_oracle_disk(workdir):
    if workdir is None:
        return
    total = 0
    for parent, _dirs, files in os.walk(workdir):
        for filename in files:
            try:
                total += (Path(parent) / filename).stat().st_size
            except FileNotFoundError:
                continue
            if total > MAX_ORACLE_DISK:
                raise ValueError("temporary oracle directory exceeds 512 MiB")


def capture(port, raw_output, workdir):
    profile_id = uuid.UUID("7b2e4f36-82f3-41bf-86c4-a1a04db46c58")
    packets = []
    registries = []
    tags = []
    small = []
    compression = None
    state = "login"
    play_login = False
    play_keep_alive = False
    total = 0
    deadline = time.monotonic() + DEADLINE_SECONDS
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.settimeout(5)
        send_packet(sock, 0, varint(PROTOCOL) + string("localhost") + struct.pack(">H", port) + varint(2))
        send_packet(sock, 0, string("WireProbe") + profile_id.bytes)
        while time.monotonic() < deadline:
            check_oracle_disk(workdir)
            try:
                packet_id, body = read_packet(sock, compression)
            except socket.timeout:
                continue
            total += len(body)
            if total > MAX_CAPTURE:
                raise ValueError("capture exceeds 32 MiB")
            packets.append({"state": state, **witness(packet_id, body)})
            if raw_output is not None:
                raw_output.write(json.dumps({"state": state, "id": packet_id, "body_hex": body.hex()}) + "\n")
            if packet_id == IDS[state].get("disconnect"):
                raise RuntimeError(f"server disconnected in {state}: {body[:400]!r}")
            if state == "login":
                if packet_id == IDS[state]["compression"]:
                    compression = Cursor(body).number()
                elif packet_id == IDS[state]["finished"]:
                    send_packet(sock, 3, compression=compression)
                    state = "configuration"
                    send_packet(sock, 0, client_information(), compression)
            elif state == "configuration":
                if packet_id == IDS[state]["known_packs"]:
                    send_packet(sock, 7, b"\0", compression)
                elif packet_id == IDS[state]["registry"]:
                    name, entries, content_digest = registry_names(body)
                    registries.append({"name": name, "entries": entries, "content_sha256": content_digest, **witness(packet_id, body)})
                elif packet_id == IDS[state]["tags"]:
                    tags.append({"registries": tag_order(body), **witness(packet_id, body)})
                elif packet_id == IDS[state]["keep_alive"]:
                    send_packet(sock, 4, body, compression)
                elif packet_id == IDS[state]["ping"]:
                    send_packet(sock, 5, body, compression)
                elif packet_id == IDS[state]["finish"]:
                    send_packet(sock, 3, compression=compression)
                    state = "play"
                elif len(body) <= 128:
                    small.append({"id": packet_id, "body_hex": body.hex()})
            elif state == "play":
                if packet_id == IDS[state]["login"]:
                    play_login = True
                elif packet_id == IDS[state]["keep_alive"]:
                    send_packet(sock, 28, body, compression)
                    play_keep_alive = True
                    break
    if not (registries and tags and play_login and play_keep_alive):
        raise AssertionError(f"incomplete connection: registries={len(registries)}, tags={len(tags)}, play_login={play_login}, play_keep_alive={play_keep_alive}")
    return {
        "protocol": PROTOCOL,
        "server_jar_sha1": JAR_SHA1,
        "configuration_sequence": [p["id"] for p in packets if p["state"] == "configuration"],
        "registries": registries,
        "tags": tags,
        "small_configuration_bodies": small,
        "play_login": play_login,
        "play_keep_alive": play_keep_alive,
    }


def negative_control(port):
    """A 776 handshake must be rejected by the 777 server before Configuration."""
    with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
        sock.settimeout(5)
        send_packet(sock, 0, varint(776) + string("localhost") + struct.pack(">H", port) + varint(2))
        send_packet(sock, 0, string("WrongEra") + uuid.UUID(int=0).bytes)
        packet_id, _body = read_packet(sock, None)
        if packet_id != IDS["login"]["disconnect"]:
            raise AssertionError(f"wrong-protocol control received Login id {packet_id}, expected disconnect")
    print("negative control: protocol 776 Login rejected by the 777 server")


def compare(actual, saved):
    for key in ("protocol", "server_jar_sha1", "configuration_sequence", "small_configuration_bodies", "play_login", "play_keep_alive"):
        if actual[key] != saved[key]:
            raise AssertionError(f"{key} changed")
    if len(actual["registries"]) != len(saved["registries"]):
        raise AssertionError("registry packet count changed")
    for index, (new, old) in enumerate(zip(actual["registries"], saved["registries"])):
        for key in ("name", "entries", "content_sha256", "id", "length"):
            if new[key] != old[key]:
                raise AssertionError(f"registry {index} {new['name']} changed {key}")
        if new["name"] not in RAW_UNSTABLE_REGISTRIES and new["sha256"] != old["sha256"]:
            raise AssertionError(f"registry {index} {new['name']} changed raw bytes")
    if len(actual["tags"]) != len(saved["tags"]):
        raise AssertionError("tag packet count changed")
    for index, (new, old) in enumerate(zip(actual["tags"], saved["tags"])):
        for key in ("id", "length"):
            if new[key] != old[key]:
                raise AssertionError(f"tag packet {index} changed {key}")
        actual_registries = {entry["name"]: entry for entry in new["registries"]}
        saved_registries = {entry["name"]: entry for entry in old["registries"]}
        if actual_registries.keys() != saved_registries.keys():
            raise AssertionError(f"tag packet {index} registry set changed")
        for name, entry in actual_registries.items():
            prior = saved_registries[name]
            if set(entry["tags"]) != set(prior["tags"]):
                raise AssertionError(f"tag names changed for {name}")
            if entry["membership_sha256"] != prior["membership_sha256"]:
                raise AssertionError(f"tag membership changed for {name}")


def run_oracle(workdir):
    workdir.joinpath("eula.txt").write_text("eula=true\n")
    workdir.joinpath("server.properties").write_text(
        f"server-port={PORT}\nserver-ip=\nonline-mode=false\n"
        "white-list=false\nenforce-whitelist=false\n"
        "enforce-secure-profile=false\nnetwork-compression-threshold=256\n"
        "level-type=minecraft:flat\nlevel-seed=263777\nview-distance=2\n"
        "simulation-distance=2\nmax-players=2\nspawn-protection=0\n"
        "pause-when-empty-seconds=0\n"
    )
    shutil.copyfile(JAR, workdir / "server.jar")
    name = f"lodestone-263-capture-{os.getpid()}"
    command = [
        "container", "run", "-d", "--rm", "--name", name,
        "--memory", "2g", "--cpus", "2",
        "--ulimit", "fsize=134217728:134217728", "-p", f"{PORT}:{PORT}",
        "-v", f"{workdir}:/work", "-w", "/work", "eclipse-temurin:25-jdk",
        "java", "-Xms256M", "-Xmx1G", "-jar", "server.jar", "nogui",
    ]
    subprocess.run(command, check=True, capture_output=True, text=True, timeout=30)
    return name


def await_oracle(workdir):
    end = time.monotonic() + 90
    while time.monotonic() < end:
        check_oracle_disk(workdir)
        log = workdir / "logs/latest.log"
        if log.exists():
            contents = log.read_text(errors="replace")
            if "Done (" in contents:
                return
            if "FAILED TO BIND TO PORT" in contents or "Exception" in contents:
                raise RuntimeError(f"26.3 oracle startup failed:\n{contents[-4000:]}")
        time.sleep(1)
    raise TimeoutError("local 26.3 oracle did not report readiness within 90 seconds")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--record", action="store_true", help="write the compact committed witness")
    parser.add_argument("--raw-output", type=Path, help="write full packet bodies as temporary JSONL")
    parser.add_argument("--external-port", type=int, help="connect to an already running local server")
    parser.add_argument("--negative-control", action="store_true", help="require a wrong-protocol login to fail first")
    args = parser.parse_args()
    jar_hash_state = hashlib.sha1()
    with JAR.open("rb") as jar_file:
        for chunk in iter(lambda: jar_file.read(1024 * 1024), b""):
            jar_hash_state.update(chunk)
    jar_hash = jar_hash_state.hexdigest()
    if jar_hash != JAR_SHA1:
        raise ValueError("cached 26.3 server jar SHA-1 differs from the official release")
    with tempfile.TemporaryDirectory(prefix="lodestone-263-capture-") as directory:
        oracle = None
        try:
            if args.external_port is None:
                oracle = run_oracle(Path(directory))
                await_oracle(Path(directory))
            if args.negative_control:
                negative_control(args.external_port or PORT)
            raw = args.raw_output.open("w") if args.raw_output else None
            try:
                result = capture(args.external_port or PORT, raw, None if args.external_port else Path(directory))
            except Exception:
                if oracle:
                    log = Path(directory) / "logs/latest.log"
                    if log.exists():
                        print(log.read_text(errors="replace")[-4000:], file=sys.stderr)
                raise
            finally:
                if raw:
                    raw.close()
        finally:
            if oracle:
                subprocess.run(["container", "rm", "-f", oracle], capture_output=True, timeout=30)
    if args.record:
        FIXTURE.write_text(json.dumps(result, indent=2) + "\n")
        print(f"wrote {FIXTURE}")
    else:
        expected = json.loads(FIXTURE.read_text())
        compare(result, expected)
        print("26.3 connection matches committed witness")
    print(f"captured {len(result['registries'])} registries, {len(result['tags'])} tag packets, Play login and keep-alive")


if __name__ == "__main__":
    main()
