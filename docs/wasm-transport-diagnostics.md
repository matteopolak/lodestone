# Wasm MessagePort transport diagnostics

## What it is

`lodestone_net::MessagePortTransport` carries the protocol byte stream between the page-side client and the integrated-server worker. Opt-in debug diagnostics expose byte flow and credit stalls so a stuck join can be attributed to backpressure, a slow peer read, or something else.

## How it works

When the `message_port` log target accepts `DEBUG` at construction, each endpoint owns a `ByteTransportDiagnostics`. It counts bytes posted to the peer, received into the bounded inbox, and drained by the local reader, and records the longest span where `poll_write` had no send credit.

A browser timer logs the cumulative counters, current credits and write-pending durations at most once per second per endpoint; I/O and credit events may log earlier within that limit. The heartbeat keeps reporting a parked writer's wait age. The timer is cleared with the transport.

Credits are local: send credit is the allowance the peer granted; receive credit is the allowance left for incoming bytes. Diagnostics observe only; they never change framing, partial writes or grants.

## How to change it

- Counter and rate-limit semantics: `ByteTransportDiagnostics` in `lodestone_net::inbox` (tested natively).
- Where bytes are posted, received and drained, and the logged fields: `MessagePortTransport` in `lodestone_net::worker_web` (Wasm-only).
- Keep diagnostics optional so ordinary endpoints start no timer and read no clock.

## Configuration

Standalone page: `?log=debug`. Embedding API: `logLevel: "debug"`. Lower levels leave diagnostics unallocated.

## Dependencies

Browser `MessagePort` events, Tokio `AsyncRead`/`AsyncWrite`, `ByteCreditWindow`/`ByteTransportDiagnostics` in `lodestone_net::inbox`, and `lodestone_time::Instant` for the browser-safe clock.
