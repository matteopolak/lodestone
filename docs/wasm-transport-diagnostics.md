# Wasm MessagePort transport diagnostics

## What it is

`lodestone_net::MessagePortTransport` moves the protocol byte stream between the page-side client and the integrated-server worker. Opt-in debug diagnostics expose endpoint byte flow and credit stalls so a join that stops progressing can be separated into transport backpressure, peer-read delay, or work elsewhere in the join path.

## How it works

Each transport endpoint owns its own `ByteTransportDiagnostics` when the `message_port` debug target is enabled through the `log` facade at construction. It counts bytes posted to the peer, bytes received into the bounded inbox, and bytes drained by the local protocol reader. It also records the longest interval during which `poll_write` had no send credit. A debug-only browser timer samples the cumulative counters, current send and receive credit, and current and maximum write-pending durations at most once per second per endpoint. I/O and credit events can produce an earlier line when the one-second rate limit allows it. This heartbeat means a sustained zero-credit wait continues to report its age even when the writer is parked and receives no new events. The timer is cleared with the transport.

The credit values describe the local endpoint: send credit is the remaining allowance granted by the peer; receive credit is the remaining allowance for incoming bytes. A write-pending duration begins when send credit is exhausted and ends when credit returns and a write succeeds. Counters are diagnostic observations only; they do not change framing, partial-write behavior, or credit grants.

## How to change it

Change `ByteTransportDiagnostics` in `lodestone_net::inbox` for counter and rate-limit semantics. Change `MessagePortTransport` in `lodestone_net::worker_web` for the points where bytes are posted, received, and drained, or for the fields included in the debug line. Keep diagnostics optional so ordinary transport instances do not start a timer, take clock readings, or update counters. The module is Wasm-only, while the pure counter and rate-limit tests run natively through `inbox`.

## Configuration

Diagnostics are enabled when the `message_port` log target accepts `DEBUG` at endpoint construction. The browser standalone page selects the log level with `?log=debug`; the embedding API accepts `logLevel: "debug"`. Lower levels leave the per-endpoint diagnostics unallocated.

## Dependencies

The transport uses browser `MessagePort` events and Tokio's `AsyncRead`/`AsyncWrite` interface. `ByteCreditWindow` and `ByteTransportDiagnostics` in `lodestone_net::inbox` hold the pure bounded-credit and counter logic; `lodestone_time::Instant` supplies the browser-safe clock.
