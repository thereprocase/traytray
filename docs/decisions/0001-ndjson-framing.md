# 0001 — Newline-delimited JSON frames instead of HTTP + WebSocket

Status: accepted, 2026-10-03

## Context

The first plan used HTTP for writes, a WebSocket for host-to-app events, and an HTTP `POST /pair`.

## Decision

Use one ordered byte stream per connection carrying newline-delimited JSON frames, in both
directions. The same frames run over a Unix socket, a Windows named pipe, and tailnet TCP.
Pairing is a `pair_request` / `pair_code` frame exchange on a not-yet-authenticated
connection.

## Reasons

- No browser ever talks to the host. Tier-2 web panels talk to their own app, so WebSocket's
  browser compatibility buys nothing.
- Tailnet traffic is already encrypted and authenticated by WireGuard, so TLS on top adds
  nothing at this layer.
- A single ordered channel per connection removes the race between HTTP writes and
  WebSocket events. With a monotonic `rev`, ordering is fully specified.
- Fewer dependencies. The core needs no HTTP or WebSocket crates. The KDE plugin uses
  `QLocalSocket`/`QTcpSocket` directly, and the .NET SDK uses `NamedPipeClientStream` and
  `TcpClient`.

## Consequences

- Frames have a hard size limit (256 KB). The reader is bounded and rejects longer lines
  without buffering them.
- JSON strings escape newlines, so a frame never contains a raw newline.
- If tier-2 panels ever need a host API, that is a separate ADR.
