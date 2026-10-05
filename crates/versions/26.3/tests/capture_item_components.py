#!/usr/bin/env python3
"""Captures how the official 26.3 server writes item component patches.

Starts the cached 26.3 server jar in a container, joins it as an offline
player, and for each stack in ``CASES`` clears the inventory, has the server
``give`` the stack over RCON, and records the raw stack bytes of the slot
update that follows (count, item id, component patch). Each case carries a
single component, so the patch bytes do not depend on map iteration order.

Run ``python3 capture_item_components.py`` to rewrite
``fixtures/item_components_26_3.json``. With ``--saved`` it instead has the
server place every stack of ``SAVED_CASES`` in one chest, saves the world, and
writes that chunk's decompressed NBT to ``fixtures/item_components_26_3_chunk.nbt``
with the slot map in ``fixtures/item_components_26_3_saved.json``: the
save-file form of the same components. This tool deliberately does not call
Lodestone code: the fixtures are the server's own encoding.
"""

import json
import select
import socket
import struct
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import capture_connection as cc  # noqa: E402

FIXTURE = cc.HERE / "fixtures/item_components_26_3.json"
RCON_PORT = 25678
PLAYER = "WireProbe"

# name -> give argument. One component per stack.
CASES = {
    "damage": "minecraft:diamond_pickaxe[minecraft:damage=250]",
    "enchantments": "minecraft:diamond_pickaxe[minecraft:enchantments={\"minecraft:efficiency\":4}]",
    "stored_enchantments": "minecraft:enchanted_book[minecraft:stored_enchantments={\"minecraft:mending\":1}]",
    "custom_name_plain": "minecraft:diamond_pickaxe[minecraft:custom_name='Lodey']",
    "custom_name_styled": "minecraft:diamond_sword[minecraft:custom_name={text:'Red',color:'red',italic:false}]",
    "lore": "minecraft:stick[minecraft:lore=['line one','line two']]",
    "dyed_color": "minecraft:leather_helmet[minecraft:dyed_color=1193046]",
    "repair_cost": "minecraft:diamond_pickaxe[minecraft:repair_cost=7]",
    "potion": "minecraft:potion[minecraft:potion_contents={potion:'minecraft:poison'}]",
    "potion_named": "minecraft:potion[minecraft:potion_contents={potion:'minecraft:strength',custom_name:'lodey'}]",
    "instrument": "minecraft:goat_horn[minecraft:instrument='minecraft:sing_goat_horn']",
    "plain": "minecraft:diamond",
}

# The save-file cases: every wire case that the server can put in a chest,
# plus the components only a saved stack exercises.
SAVED_CASES = {
    **CASES,
    "custom_data": "minecraft:stick[minecraft:custom_data={lodestone:1b,name:'x'}]",
    "writable_book": "minecraft:writable_book[minecraft:writable_book_content={pages:['first','second']}]",
    "written_book": "minecraft:written_book[minecraft:written_book_content={title:'T',author:'A',pages:['page one'],resolved:true}]",
    "stack_of_many": "minecraft:diamond 37",
}
SAVED_FIXTURE = cc.HERE / "fixtures/item_components_26_3_saved.json"
CHUNK_FIXTURE = cc.HERE / "fixtures/item_components_26_3_chunk.nbt"
CHEST = (0, -60, 0)

SET_SLOT = 20
SET_PLAYER_INVENTORY = 110
KEEP_ALIVE_IN, KEEP_ALIVE_OUT = 45, 28


class Rcon:
    def __init__(self, port, password):
        self.sock = socket.create_connection(("127.0.0.1", port), timeout=10)
        self.next_id = 1
        self.request(3, password)

    def request(self, kind, payload):
        request_id = self.next_id
        self.next_id += 1
        body = struct.pack("<ii", request_id, kind) + payload.encode() + b"\0\0"
        self.sock.sendall(struct.pack("<i", len(body)) + body)
        size = struct.unpack("<i", cc.recv_exact(self.sock, 4))[0]
        reply = cc.recv_exact(self.sock, size)
        reply_id = struct.unpack("<i", reply[:4])[0]
        if reply_id == -1:
            raise RuntimeError("RCON authentication failed")
        return reply[8:-2].decode(errors="replace")

    def command(self, text):
        return self.request(2, text)


def run_oracle(workdir):
    workdir.joinpath("eula.txt").write_text("eula=true\n")
    workdir.joinpath("server.properties").write_text(
        f"server-port={cc.PORT}\nserver-ip=\nonline-mode=false\n"
        "white-list=false\nenforce-whitelist=false\n"
        "enforce-secure-profile=false\nnetwork-compression-threshold=256\n"
        "level-type=minecraft:flat\nlevel-seed=263777\nview-distance=2\n"
        "simulation-distance=2\nmax-players=2\nspawn-protection=0\n"
        "pause-when-empty-seconds=0\n"
        f"enable-rcon=true\nrcon.port={RCON_PORT}\nrcon.password=lodestone\n"
    )
    cc.shutil.copyfile(cc.JAR, workdir / "server.jar")
    name = f"lodestone-263-items-{cc.os.getpid()}"
    command = [
        "container", "run", "-d", "--rm", "--name", name,
        "--memory", "2g", "--cpus", "2",
        "--ulimit", "fsize=134217728:134217728",
        "-p", f"{cc.PORT}:{cc.PORT}", "-p", f"{RCON_PORT}:{RCON_PORT}",
        "-v", f"{workdir}:/work", "-w", "/work", "eclipse-temurin:25-jdk",
        "java", "-Xms256M", "-Xmx1G", "-jar", "server.jar", "nogui",
    ]
    subprocess.run(command, check=True, capture_output=True, text=True, timeout=30)
    return name


def join(port):
    sock = socket.create_connection(("127.0.0.1", port), timeout=5)
    sock.settimeout(5)
    profile_id = uuid.UUID("7b2e4f36-82f3-41bf-86c4-a1a04db46c58")
    cc.send_packet(sock, 0, cc.varint(cc.PROTOCOL) + cc.string("localhost") + struct.pack(">H", port) + cc.varint(2))
    cc.send_packet(sock, 0, cc.string(PLAYER) + profile_id.bytes)
    compression = None
    state = "login"
    deadline = time.monotonic() + cc.DEADLINE_SECONDS
    while time.monotonic() < deadline:
        packet_id, body = cc.read_packet(sock, compression)
        if packet_id == cc.IDS[state].get("disconnect"):
            raise RuntimeError(f"server disconnected in {state}: {body[:400]!r}")
        if state == "login":
            if packet_id == cc.IDS[state]["compression"]:
                compression = cc.Cursor(body).number()
            elif packet_id == cc.IDS[state]["finished"]:
                cc.send_packet(sock, 3, compression=compression)
                state = "configuration"
                cc.send_packet(sock, 0, cc.client_information(), compression)
        elif state == "configuration":
            if packet_id == cc.IDS[state]["known_packs"]:
                cc.send_packet(sock, 7, b"\0", compression)
            elif packet_id == cc.IDS[state]["keep_alive"]:
                cc.send_packet(sock, 4, body, compression)
            elif packet_id == cc.IDS[state]["ping"]:
                cc.send_packet(sock, 5, body, compression)
            elif packet_id == cc.IDS[state]["finish"]:
                cc.send_packet(sock, 3, compression=compression)
                state = "play"
        elif packet_id == cc.IDS["play"]["login"]:
            return sock, compression
    raise TimeoutError("did not reach Play")


def stack_bytes(packet_id, body):
    cursor = cc.Cursor(body)
    if packet_id == SET_SLOT:
        cursor.number()  # container id
        cursor.number()  # state id
        cursor.take(2)  # slot
    else:
        cursor.number()  # slot
    return body[cursor.at:]


def await_stack(sock, compression, seconds=10):
    """The stack bytes of the next non-empty slot update."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if not select.select([sock], [], [], 0.2)[0]:
            continue
        packet_id, body = cc.read_packet(sock, compression)
        if packet_id == KEEP_ALIVE_IN:
            cc.send_packet(sock, KEEP_ALIVE_OUT, body, compression)
        elif packet_id in (SET_SLOT, SET_PLAYER_INVENTORY):
            stack = stack_bytes(packet_id, body)
            if stack != b"\0":
                return stack
    raise TimeoutError("no slot update arrived")


def drain(sock, compression, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if not select.select([sock], [], [], 0.2)[0]:
            continue
        packet_id, body = cc.read_packet(sock, compression)
        if packet_id == KEEP_ALIVE_IN:
            cc.send_packet(sock, KEEP_ALIVE_OUT, body, compression)


def region_chunk(region, cx, cz):
    """The decompressed NBT of chunk (cx, cz) in a region file."""
    data = region.read_bytes()
    index = 4 * ((cx & 31) + (cz & 31) * 32)
    location = int.from_bytes(data[index:index + 3], "big")
    if location == 0:
        raise ValueError(f"chunk {cx},{cz} is not in {region}")
    start = location * 4096
    length = int.from_bytes(data[start:start + 4], "big")
    if data[start + 4] != 2:
        raise ValueError("chunk is not zlib-compressed")
    return cc.zlib.decompress(data[start + 5:start + 4 + length])


def capture_saved():
    with tempfile.TemporaryDirectory(prefix="lodestone-263-saved-") as directory:
        workdir = Path(directory)
        oracle = run_oracle(workdir)
        try:
            cc.await_oracle(workdir)
            rcon = Rcon(RCON_PORT, "lodestone")
            x, y, z = CHEST
            rcon.command("forceload add 0 0")
            reply = rcon.command(f"setblock {x} {y} {z} minecraft:chest")
            if "Changed" not in reply:
                raise RuntimeError(f"setblock failed: {reply}")
            slots = {}
            for slot, (name, spec) in enumerate(SAVED_CASES.items()):
                item, _, count = spec.partition(" ")
                reply = rcon.command(f"item replace block {x} {y} {z} container.{slot} with {item} {count or 1}")
                if "Replaced" not in reply:
                    raise RuntimeError(f"item replace {spec!r} failed: {reply}")
                slots[name] = {"slot": slot, "give": spec}
            reply = rcon.command("save-all flush")
            time.sleep(2)
            if "Saved the game" not in reply:
                raise RuntimeError(f"save-all failed: {reply}")
            region = workdir / "world/dimensions/minecraft/overworld/region/r.0.0.mca"
            chunk = region_chunk(region, 0, 0)
        except Exception:
            time.sleep(2)
            log = workdir / "logs/latest.log"
            if log.exists():
                print(log.read_text(errors="replace")[-4000:], file=sys.stderr)
            raise
        finally:
            subprocess.run(["container", "rm", "-f", oracle], capture_output=True, timeout=30)
    CHUNK_FIXTURE.write_bytes(chunk)
    SAVED_FIXTURE.write_text(json.dumps({
        "protocol": cc.PROTOCOL,
        "server_jar_sha1": cc.JAR_SHA1,
        "chest": list(CHEST),
        "note": "chest slots filled with `item replace`; the chunk is the saved, decompressed region entry",
        "slots": slots,
    }, indent=2) + "\n")
    print(f"wrote {CHUNK_FIXTURE} ({len(chunk)} bytes) and {SAVED_FIXTURE}")


def main():
    if "--saved" in sys.argv:
        capture_saved()
        return
    with tempfile.TemporaryDirectory(prefix="lodestone-263-items-") as directory:
        workdir = Path(directory)
        oracle = run_oracle(workdir)
        try:
            cc.await_oracle(workdir)
            sock, compression = join(cc.PORT)
            drain(sock, compression, 3)
            rcon = Rcon(RCON_PORT, "lodestone")
            cases = {}
            for name, spec in CASES.items():
                rcon.command(f"clear {PLAYER}")
                drain(sock, compression, 1)
                reply = rcon.command(f"give {PLAYER} {spec}")
                if "Gave" not in reply:
                    raise RuntimeError(f"give {spec!r} failed: {reply}")
                cases[name] = {"give": spec, "stack_hex": await_stack(sock, compression).hex()}
                print(f"{name}: {cases[name]['stack_hex']}")
            sock.close()
        except Exception:
            time.sleep(2)
            log = workdir / "logs/latest.log"
            if log.exists():
                print(log.read_text(errors="replace")[-4000:], file=sys.stderr)
            raise
        finally:
            subprocess.run(["container", "rm", "-f", oracle], capture_output=True, timeout=30)
    FIXTURE.write_text(json.dumps({
        "protocol": cc.PROTOCOL,
        "server_jar_sha1": cc.JAR_SHA1,
        "note": "stack bytes (count, item id, component patch) of the slot update after `give`",
        "cases": cases,
    }, indent=2) + "\n")
    print(f"wrote {FIXTURE}")


if __name__ == "__main__":
    main()
