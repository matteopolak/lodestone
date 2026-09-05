#!/usr/bin/env python3
"""Capture a bounded, tick-by-tick Java-oracle trace over Source RCON.

The input is a JSON *plan* with ``scenario``, ``settle_ticks``, ``region`` and
``steps`` fields matching the output corpus, except that it has no
``provenance`` or ``observations``.  Every step must be a ``set_block`` action.
The output is an immutable expected-value artifact: the Rust consumer refuses
anything not marked ``real-java-rcon`` and never regenerates its observations.

This is intentionally not a stress runner.  It accepts at most eight edits,
sixteen probes, and sixteen observed ticks.  The game-time counter must advance
by exactly one between observations; a skipped boundary aborts capture instead
of recording an ambiguously labelled trace.
"""

import argparse
import json
import socket
import sys
import time

TYPE_AUTH = 3
TYPE_COMMAND = 2
MAX_STEPS = 8
MAX_PROBES = 16
MAX_TICKS = 16


def frame(request_id, packet_type, payload):
    body = (request_id.to_bytes(4, "little", signed=True) + packet_type.to_bytes(4, "little", signed=True)
            + payload.encode("utf-8") + b"\x00\x00")
    return len(body).to_bytes(4, "little", signed=True) + body


def exact(sock, size):
    result = bytearray()
    while len(result) < size:
        part = sock.recv(size - len(result))
        if not part:
            raise ConnectionError("RCON connection closed before a full frame arrived")
        result.extend(part)
    return bytes(result)


class Rcon:
    def __init__(self, endpoint, password):
        host, port = endpoint.rsplit(":", 1)
        self.sock = socket.create_connection((host, int(port)), timeout=5)
        self.sock.settimeout(5)
        self.next_id = 1
        self.command(password, TYPE_AUTH)

    def command(self, command, packet_type=TYPE_COMMAND):
        request_id = self.next_id
        self.next_id += 1
        self.sock.sendall(frame(request_id, packet_type, command))
        size = int.from_bytes(exact(self.sock, 4), "little", signed=True)
        if size < 10 or size > 4 * 1024 * 1024:
            raise ValueError(f"invalid RCON frame length {size}")
        body = exact(self.sock, size)
        response_id = int.from_bytes(body[:4], "little", signed=True)
        if response_id != request_id:
            raise PermissionError("RCON authentication or response id failed")
        return body[8:-2].decode("utf-8")

    def close(self):
        self.sock.close()


def game_time(rcon):
    numbers = [int(token) for token in rcon.command("time query gametime").split() if token.lstrip("-").isdigit()]
    if not numbers:
        raise ValueError("time query gametime returned no integer")
    return numbers[-1]


def probe(rcon, pos, candidates):
    x, y, z = pos
    for candidate in candidates:
        answer = rcon.command(f"execute if block {x} {y} {z} {candidate}").strip()
        if answer.startswith("Test passed"):
            return candidate
        if not answer.startswith("Test failed"):
            raise ValueError(f"block-state probe returned {answer!r}, not a terminal test result")
    return None


def validate(plan):
    required = {"scenario", "settle_ticks", "region", "steps"}
    missing = required - set(plan)
    if missing:
        raise ValueError(f"plan is missing {sorted(missing)}")
    if not isinstance(plan["steps"], list) or not 1 <= len(plan["steps"]) <= MAX_STEPS:
        raise ValueError(f"plan needs 1..={MAX_STEPS} steps")
    if not isinstance(plan["region"], list) or not 1 <= len(plan["region"]) <= MAX_PROBES:
        raise ValueError(f"plan needs 1..={MAX_PROBES} probes")
    if plan["steps"][0]["tick"] != 0:
        raise ValueError("plan must begin at tick 0")
    previous = -1
    for step in plan["steps"]:
        if step["tick"] < previous or set(step["action"]) != {"kind", "pos", "state"} or step["action"]["kind"] != "set_block":
            raise ValueError("steps must be ordered set_block actions")
        previous = step["tick"]
    total = plan["steps"][-1]["tick"] + plan["settle_ticks"] + 1
    if total > MAX_TICKS:
        raise ValueError(f"plan horizon {total} exceeds {MAX_TICKS} ticks")
    for probe_spec in plan["region"]:
        if not probe_spec.get("candidates"):
            raise ValueError("every probe needs at least one candidate")
    return total


def recorded_command(argv):
    """Keep an operator's RCON password out of a committed expected corpus."""
    result = []
    redact_next = False
    for argument in argv:
        if redact_next:
            result.append("<redacted>")
            redact_next = False
        elif argument == "--password":
            result.append(argument)
            redact_next = True
        elif argument.startswith("--password="):
            result.append("--password=<redacted>")
        else:
            result.append(argument)
    return " ".join(result)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--spec", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--endpoint", default="127.0.0.1:25571")
    parser.add_argument("--password", default="lodestone")
    parser.add_argument("--minecraft-version", default="26.2")
    args = parser.parse_args()
    with open(args.spec, encoding="utf-8") as source:
        plan = json.load(source)
    total = validate(plan)
    rcon = Rcon(args.endpoint, args.password)
    try:
        baseline = game_time(rcon)
        observations = []
        for tick in range(total):
            for step in (entry for entry in plan["steps"] if entry["tick"] == tick):
                x, y, z = step["action"]["pos"]
                rcon.command(f"setblock {x} {y} {z} {step['action']['state']}")
            deadline = time.monotonic() + 5
            current = baseline
            while current == baseline:
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"game time did not advance from {baseline}")
                time.sleep(0.01)
                current = game_time(rcon)
            if current != baseline + 1:
                raise RuntimeError(f"tick boundary skipped from {baseline} to {current}; retry on a quieter oracle")
            observations.append({"tick": tick, "game_time": current,
                                 "states": [probe(rcon, entry["pos"], entry["candidates"]) for entry in plan["region"]]})
            baseline = current
    finally:
        rcon.close()
    output = {"format_version": 1, "scenario": plan["scenario"],
              "provenance": {"source": "real-java-rcon", "minecraft_version": args.minecraft_version,
                             "capture_command": recorded_command(sys.argv)},
              "settle_ticks": plan["settle_ticks"], "region": plan["region"], "steps": plan["steps"],
              "observations": observations}
    with open(args.out, "w", encoding="utf-8") as destination:
        json.dump(output, destination, indent=2)
        destination.write("\n")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TimeoutError, RuntimeError, PermissionError) as error:
        print(f"tick corpus capture failed: {error}", file=sys.stderr)
        raise SystemExit(1)
