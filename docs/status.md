# Status and handoff

Last updated 2026-10-03. Pre-alpha: nothing runs end-to-end yet.

## Done

- **Design** (docs/design.md), revised after a design review and after the M0 spikes. ADR 0001:
  newline-delimited JSON framing instead of HTTP + WebSocket.
- **Dependencies** pinned exactly; build scripts reviewed (see the `build(deps)` commit).
- **crates/proto**: frame types, limits, text sanitising, bounded framing, validation. 28 tests.
- **crates/core** (library only, no daemon yet). 104 tests. Each module was reviewed
  adversarially and fixed:
  - `store`: in-memory app state, connection binding, rev ordering, event dedupe, rate limit,
    privacy OR-ing, toast decisions, urgency roll-up, fanout pacing, capped offline tiles.
  - `pairing`: host-started pairing window, whois-verified request, 6-digit code with lockout and
    expiry, SHA-256 token storage, token + node authentication.
  - `feed`: the only file writer; per-app JSONL feed with retention, pairings.json, settings.json,
    atomic saves, 0600/0700 modes, persistence-contract test.
- **M0 spikes**: results in spikes/m0/results/, summary in docs/testlog.md (2026-10-03).

## Open items found in review (not yet fixed)

1. `FeedEntry` is defined twice (`store::FeedEntry`/`FeedKind` and `feed::FeedEntry`/`FeedRecord`).
   Unify on `feed`'s types when wiring the server.
2. Connection-level privacy reaches shells only on Status, Progress and Row items. Text, Buttons,
   Reply blocks and menus carry no flag; add `privacy` to `AppView` in proto.
3. `store::apply_state` returns validation errors that can echo item ids from the state document.
   The persistence contract says state content is never echoed into errors; reduce to codes.
4. `pairing` types derive `Debug` while holding the live pairing code; redact before any logging
   exists.
5. Feed retention counts dismisses against the 200-entry budget; design.md says 200 events plus
   dismisses. Pick one and align the doc or the code.
6. KDE plugin: bound `QLocalSocket`'s own read buffer (`setReadBufferSize`), not only the line
   buffer.
7. The feed owner-check test has only been seen passing (sabotage could not be run as written).

## Blocked on the owner

- **Codex hooks**: need to be added and trusted in the Codex TUI by the owner.
- **M0.3** (managed Windows laptop with WSL: inbound tailnet TCP, non-admin whois, WSL↔Windows
  forwarding): needs the owner's go-ahead. Alpha-c depends on it.
- **Claude Code hooks** for the agents app: installed only as a diff the owner approves.
- **Windows builds of the core** will compile tokio's Windows dependencies (windows-sys 0.61.2,
  windows-link 0.2.1), which are locked but have not been built yet; name them at that point.

## Next (alpha-a)

1. `crates/core/src/server.rs`: Unix socket listener (0600, SO_PEERCRED), hello handshake with
   proto version and roles, per-connection frame loop, store/feed wiring, shell fanout, heartbeats.
   `traytrayd` binary with a systemd user unit.
2. `crates/sdk`: connect, hello, publish state with rev, events with ids, receive actions/replies,
   reconnect with jittered backoff.
3. Conformance suite against a real `traytrayd`: stale rev, dedupe, app cannot send shell frames
   or write another app's state, oversized frames, persistence audit with a sabotage run.
4. KDE shell: plasmoid with the bundled C++ plugin (spike code in spikes/m0/kde-plugin), tier 0/1
   rendering, `X-Plasma-NotificationAreaCategory` metadata, toasts via
   org.freedesktop.Notifications, "Host not running" state.
5. Agents app: resident daemon, hook script per design.md (PermissionRequest primary, waiting clears
   on any later event, sync vs async per event), liveness by PID, reply safety rules.

Alpha-b (Windows shell, .NET SDK, robo-rightclick 1.1) and alpha-c (pairing across machines)
follow; see docs/design.md, Milestones.

## Working rules

See CLAUDE.md. In short: public repo, no personal or machine identifiers; runtime claims cite
docs/testlog.md; every security or persistence rule is seen failing once; no new crate without
approval; spike harnesses start tmux with `-f /dev/null` and never touch the user's tmux server.
