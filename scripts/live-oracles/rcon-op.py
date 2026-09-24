#!/usr/bin/env python3
"""Grant `op` to a named player on a live oracle over Source RCON.

Shared by creative.sh, survival.sh and terrain.sh so the interactive account
can use client-side testing affordances (e.g. `/givedebug`) that vanilla
refuses to a non-op player.

# Why RCON `op <name>` rather than writing `ops.json` before start

Offline mode derives the account UUID from the *username*
(`OfflinePlayer:<name>`), so in principle `ops.json` could be pre-populated
without the player ever having joined. But that requires reproducing
Mojang's offline-UUID algorithm (an MD5-based, version-3 `UUID` over
`"OfflinePlayer:" + name`) correctly in *this* script — exactly the kind of
hand-rolled reimplementation this repo's CLAUDE.md warns burns whole
sessions when it is subtly wrong, and there is no way to verify it offline.

`op <name>` over RCON needs none of that: vanilla's own command already
resolves an offline player's UUID from the name server-side (and has done so
since 1.7.6, well before RCON existed on 26.2), and it works whether or not
the named player has ever joined. The server is the authority on its own
UUID derivation, so let it do the deriving.

# The one-`read()`-per-request constraint

Vanilla's RCON server performs exactly one `read()` per incoming request and
closes the connection if that single read doesn't contain the whole frame
(`pktsize != read - 4`). `sock.sendall(frame)` on a `frame` built as one
contiguous `bytes` object is what keeps this to one `write()` call; never
build the frame with multiple `send()` calls.

Mirrors `lodestone_testsupport::RconClient` in
`crates/lodestone-testsupport/src/lib.rs` (Rust, used by the live gates) —
that client cannot be reused here because it is compiled into test binaries,
not available to a pre-`cargo build` shell script that just wants to op an
account before the client ever connects.
"""

import socket
import sys

TYPE_AUTH = 3
TYPE_COMMAND = 2


def build_frame(request_id: int, packet_type: int, payload: str) -> bytes:
    body = (
        request_id.to_bytes(4, "little", signed=True)
        + packet_type.to_bytes(4, "little", signed=True)
        + payload.encode("utf-8")
        + b"\x00\x00"
    )
    return len(body).to_bytes(4, "little", signed=True) + body


def read_response(sock: socket.socket) -> tuple[int, str]:
    length_bytes = recv_exact(sock, 4)
    length = int.from_bytes(length_bytes, "little", signed=True)
    if length < 10:
        raise ValueError(f"short RCON response (length={length})")
    body = recv_exact(sock, length)
    response_id = int.from_bytes(body[0:4], "little", signed=True)
    payload = body[8:-2].decode("utf-8", errors="replace")
    return response_id, payload


def recv_exact(sock: socket.socket, n: int) -> bytes:
    chunks = []
    remaining = n
    while remaining > 0:
        chunk = sock.recv(remaining)
        if not chunk:
            raise ConnectionError("RCON connection closed before a full frame arrived")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def main() -> int:
    if len(sys.argv) != 5:
        print(
            f"usage: {sys.argv[0]} <host> <port> <password> <command...>",
            file=sys.stderr,
        )
        return 2
    host, port_str, password, command = sys.argv[1:5]
    port = int(port_str)

    with socket.create_connection((host, port), timeout=10) as sock:
        sock.settimeout(10)

        auth_id = 1
        sock.sendall(build_frame(auth_id, TYPE_AUTH, password))
        response_id, _ = read_response(sock)
        if response_id != auth_id:
            print("RCON authentication failed", file=sys.stderr)
            return 1

        cmd_id = 2
        sock.sendall(build_frame(cmd_id, TYPE_COMMAND, command))
        response_id, payload = read_response(sock)
        if response_id != cmd_id:
            print(f"RCON response id mismatch (got {response_id})", file=sys.stderr)
            return 1
        print(payload.strip() or "(empty response)")
        return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, ConnectionError) as exc:
        print(f"RCON error: {exc}", file=sys.stderr)
        raise SystemExit(1)
