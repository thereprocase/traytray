# Traytray design

Status: plan for alpha. Nothing described here is verified unless docs/testlog.md says so.

## What it is

Every desktop runs **one host** that owns **one tray icon** and exposes an **API**. Apps that
support it don't create their own tray icon. They publish into the host instead: status,
progress, messages, a reply box, buttons. The host can also collect other apps' tray icons
into a drawer.

| Topic | Decision |
|---|---|
| Machines | Cross-platform, one host per desktop. No central hub. |
| Collect | Both: an API for apps that support it, and a drawer for other tray icons. |
| Audience | Mostly the author's own tools, open to others. `v0.x` makes no stability promise. |
| Remote apps | Over a Tailscale tailnet, after a one-time pairing per app and host. |
| Actions | The app runs them. The host only relays. |
| Stack | Rust core plus native shells. |
| Rendering | Three tiers: menu items, a widget kit, and a web panel. |
| Memory | The host keeps a short feed on disk. |
| Alpha apps | Agent sessions (Claude Code, Codex) and robo-rightclick. |
| Alerting | The app sets urgency and the host maps it. One global mute plus a volume per app. |

## The thin-host rule

The host **draws, remembers briefly, and passes things on**. It never runs app-supplied
commands, never interprets app content, and never decides for an app.

- An action click becomes `action{app, action_id, item_id, rev}` sent to the owning app. The
  app decides what it means, including whether the click is stale.
- A confirmation dialog is drawn only when the app attaches `confirm{title, body, verb}`.
- A reply is passed to the app as text. The host doesn't parse, route or store it.
- **Host-local verbs form a closed list:**
  - `open_url`: https only, shown with its origin.
  - `focus_or_launch`: Windows drawer entries only. The path comes from the OS's
    notification-icon settings, never from an app, and the confirm dialog shows it.

  Opening a folder or a local path is an action delivered to the owning app.

## Hosts and sources

- **Hosts** are desktop sessions with a tray: a Linux desktop running KDE Plasma 6, and
  Windows 11 machines.
- **Sources** are where apps run. That can be the same machine, or a WSL instance on a Windows
  machine.
- WSL can't open a Windows named pipe, so an app running in WSL is a *remote* app, even to the
  tray on the same laptop. Managed Windows machines may block inbound connections to a process
  run by a non-admin user. Spike M0.3 tests this on a real managed laptop. The fallback is
  either the host dialling out to the source, or declaring that setup unsupported. It is never
  a silent weaker-auth mode.

## Architecture

```
traytray/
  crates/proto/    message types, JSON Schemas, limits, conformance suite
  crates/core/     traytrayd: connections, state, pairing, feed, urgency roll-up
  crates/sdk/      Rust client library
  sdk/dotnet/      one dependency-free C# source file, meant to be vendored
  shells/kde/      Plasma 6 system-tray applet (QML) plus a compiled C++ QML plugin that speaks the protocol
  shells/windows/  .NET 10 WinForms tray and Gridline flyout
  apps/agents/     traytray-agents: a resident daemon per source, plus a fire-and-forget hook script
```

The core is headless. Shells connect as clients with the `shell` role, so restarting a shell
loses nothing. While the core is down, a shell shows a distinct "Host not running — Start"
state, never an empty all-clear.

## Protocol

See ADR 0001 for the choice of framing.

- **Framing:** newline-delimited JSON frames over one ordered byte stream per connection. The
  same messages run over a Unix socket, a Windows named pipe, or tailnet TCP.
- **Frames:** `hello`, `state`, `event`, `action`, `reply`, `dismiss`, `heartbeat`, `pair_*`,
  `error`.
- **Handshake:** `hello{proto_version, app_id, role}`. On a version mismatch, `hello` is
  refused with a reason, and the app keeps its own UI.
- **State:** each state frame carries the app's whole document plus a monotonic `rev`. The core
  drops any `rev` at or below the current one. Updates are coalesced per app: the latest wins,
  and shells receive at most 4 per second.
- **Events** carry an app-generated `event_id`. The core dedupes on it.
- **Time:** the host's clock orders the feed and computes "last seen". App timestamps are for
  display only.
- **Offline:** a connection is offline when its socket closes or it misses 2 heartbeats.
  Heartbeats go every 10 s.
- **Limits per app:**
  - Frame ≤ 256 KB, ≤ 200 items.
  - Events ≤ 10 per second, burst 50.
  - Every rendered string has its C0, ANSI and bidi control characters stripped and its length
    capped.
- **Local transport:** a Unix socket at `$XDG_RUNTIME_DIR/traytray.sock` (mode 0600), or a named
  pipe with a current-user DACL and a peer credential check.
- **Remote transport:**
  - TCP bound **only to the tailnet address**. It retries with backoff until that address
    exists, rebinds when it changes, and never falls back to another listener.
  - Every connection needs a bearer token **and** a Tailscale `whois` of the actual TCP peer.
    The peer must match the StableNodeID and login recorded at pairing.
  - Errors are distinct (`pairing_closed`, `token_revoked`, `node_mismatch`, `proto_mismatch`).
    The SDK turns each one into a message saying what to do.

### Local threat model

- Code running as the same user **outside a sandbox** is equivalent to the user and out of
  scope. The controls below contain bugs and limit blast radius. They are not a defence
  against malware already running as you.
- An app id is bound to the connection that registered it, for as long as that connection
  lives. An app can write only its own state and events.
- **Only `shell` connections** receive full state and may send `action`, `reply`, `dismiss` or
  `revoke`. The core records which shell sent each one.
- Agent sandboxes and flatpaks must not have the socket path mounted. A testlog entry records
  whether a sandboxed agent can connect.

### Pairing

1. **The host starts it.** You click "Pair an app…", which opens a 2-minute window. Outside the
   window, `pair_request` gets `pairing_closed`. At most one pairing can be pending at a time,
   and pairing requests are rate-limited per node and globally.
2. **You accept the request.** The host shows the requester's **whois-verified node name and
   owner** next to the name the app claims. You click Accept, can edit the display name, and
   choose which tiers to grant. The app's requested tiers are only a hint. Only then does a
   6-digit code appear.
3. **You type the code into the app.** The code is single-use and bound to that request and
   node. It expires after 2 minutes, dies after 3 wrong attempts, and is compared in constant
   time.
4. **The app receives a 256-bit token.** The host stores only its SHA-256, together with the
   StableNodeID and login (not the node key, which changes on re-auth). The app keeps the
   token in a 0600 file or the OS credential store.
5. **Names and badges.** Remote app ids are namespaced `<app>@<StableNodeID>`. Tiles show the
   name you confirmed, plus a host-drawn origin badge the app can't override.

## Render tiers

| Tier | App sends | Shown |
|---|---|---|
| 0 Menu | label, icon, items (label, action_id, checked, submenu, confirm) | right-click menu, one submenu per app |
| 1 Widget kit | `status`, `text`, `progress`, `list` (rows with row actions), `buttons`, `reply`, `confirm`, plus an optional `icon_mark` (one small glyph per app, drawn by the host in a corner of its icon) | a Gridline flyout tile per app |
| 2 Web panel (beta) | `panel_url`: loopback for local apps; for remote apps, only that app's own tailnet address | a separate hardened webview window |

## Urgency: levels and edges

- **Levels.** The `urgency` on state items (`quiet` / `notice` / `needs_you` / `alert`) is
  the only input to icon colour, badge and pin.
  - The icon shows the worst level across online apps.
  - Offline apps' items stay visible but are marked stale. Together they count as one `notice`
    ("X offline since HH:MM").
- **Edges.** A toast fires only from an `event` with urgency `needs_you` or `alert`, or with
  `notice` plus `toast: true`. Toasts are deduped by `event_id`, so re-sending state never
  re-toasts. Nothing about toasts is persisted.
- **Acknowledge.** It silences the toast and badge on that host, and sends
  `dismiss{item_id}` to the app if the app is connected. A dismiss is never queued for replay.
  The pin and the level stay until the app lowers them.
- **Mute and per-app volume** are the user's and override everything.
- **Toast text** is the confirmed app name plus the item title. For `private` items it is
  "1 item needs you".

## Persistence contract

The core writes exactly three things:

1. **Per-app feed (JSONL):** the last 200 non-ephemeral events, plus dismisses of
   non-ephemeral items.
2. **Pairing records:** token hash, StableNodeID, login, confirmed name, granted tiers.
3. **Host settings.**

State documents are memory-only. They are never written to disk, and never echoed into logs or
error bodies. `ephemeral` and `private` can be set per item, per event and per connection.
While anything ephemeral is held, the core and shells suppress their own crash and log files.
Unpairing deletes that app's feed. Every app has "Clear history".

## Drawer (may slip to beta)

- **KDE:** the core registers as a StatusNotifierHost with Plasma's watcher, so it gets live
  items. "Move to Plasma's overflow" is a user-clicked shell action that goes through
  plasmashell's scripting interface. The previous value is backed up first. It is never an
  API route.
- **Windows:** the drawer lists the notification-icon settings entries. **Demote** ships only if
  spike M0.4 shows it survives an Explorer restart and a sign-out. **Focus/launch** uses the
  host-local verb.

## Alpha app 1: agent sessions

`traytray-agents` is a resident user service on each source. It owns:
- the session table
- its connections to hosts and their tokens
- whole-state publishing
- replies and opens
- liveness checks

Hook scripts only feed it:

- **The hook is one script at a fixed path.** It runs `async`, has a 2 s timeout, and writes
  one line to the daemon's local socket. It always exits 0 and prints nothing. If the daemon is
  down, the event is dropped. A dead daemon can never slow down or steer a session.
- **Hook config is executable code.** Changes to Claude Code settings and Codex `hooks.json`
  are diffs the user approves. Codex hook trust is granted by the user after review, never
  bypassed.
- **Claude Code events:** SessionStart, UserPromptSubmit, PostToolUse (clears waiting),
  Notification (by `notification_type`), PermissionDenied, Stop, StopFailure, SessionEnd.
- **Codex:** hooks are the primary source. For sessions without hooks, the daemon reads the
  tmux pane footer instead:
  - `Working (` → running
  - `Create a plan?` → waiting
  - a goal line without a Working footer → idle
- **Liveness:** the daemon checks recorded PIDs. A session that died without SessionEnd becomes
  `ended (lost)`.
- **Urgency map:**

  | Signal | Level | Toast |
  |---|---|---|
  | permission_prompt, elicitation dialogs, Codex PermissionRequest | needs_you | yes, once per waiting episode |
  | StopFailure | needs_you | yes |
  | idle_prompt, Stop | notice | no |
  | running | quiet | no |
  | ended | quiet; the row drops off later | no |
  | auth_success, quota_*, elicitation_complete/response | ignored | no |
  | headless `-p` runs and subagent events | one quiet "N background agents" row | never |

- **Tiles** show each session's `permission_mode`.

### Reply safety (enforced by the agents app)

1. **Host allowlist.** A reply is accepted only from hosts on the app's own `may_reply` list,
   which by default holds only the local host. A remote host is added on the app's own
   machine. Replying into a `bypassPermissions` or `dontAsk` session from a remote host needs a
   separate opt-in for that host.
2. **Pane check.** At registration the app records `{pane_id, pane_pid, pane_current_command}`.
   Just before sending, it re-reads them and refuses (showing Open instead) unless the pane
   still hosts that session's process.
3. **No reply box during permission prompts or elicitation dialogs.** The tile shows Open
   instead.
4. **Staleness.** A reply carries the `rev` it was typed against. If the session has moved on,
   the reply is rejected.
5. **Delivery.** Control characters are stripped and the length is capped. The text is pasted
   as one bracketed block (`tmux load-buffer` + `paste-buffer -p`), followed by Enter.
6. **Logging.** The app logs replies locally as length and hash only. The host logs nothing.

### Work machines

A source can be marked **read-only**. It publishes to other hosts as `ephemeral` items, and its
`may_reply` list is fixed to its own machine's host, with no setting that widens it.

## Alpha app 2: robo-rightclick (Windows)

- **Client.** `sdk/dotnet` is vendored into robo-rightclick as a single dependency-free source
  file, pinned to a Traytray commit. An ADR there records the exception.
- **Handover** works in both directions, with hysteresis so the icon doesn't flicker:
  - robo-rightclick drops its own icon only after the host has acknowledged its first state
    frame **and** reports at least one visible shell.
  - It restores its own icon after 2 s with no visible shell, or after 2 missed heartbeats.
  - It hands the icon back to the host only after 10 s of a stable host.
  - On a protocol version mismatch it keeps its own icon.
- **Ephemeral is per job,** as it is in robo-rightclick: each job's rows and events carry that
  job's own `ephemeral` and `private` flags.
- **The menu keeps everything** robo-rightclick's own tray menu has today:
  - Jobs, Pause all and Resume all
  - the Ephemeral mode toggle
  - Settings
  - Open logs (normal mode only)
  - the hotkey status line
  - Exit, with a confirmation that is attached only while jobs exist
- **Urgency:**

  | Outcome | Level / event |
  |---|---|
  | running | quiet |
  | Done | notice, with `toast` = the app's notifyOnComplete setting |
  | DoneWithErrors, Failed, refusals-only | needs_you + toast |
  | damaging cancel, failed cut with sources left in place | alert + toast |
  | conflict waiting | needs_you level only, no toast (robo-rightclick's own dialog is already on screen) |

## Milestones

Each milestone is gated, and its results are recorded in docs/testlog.md.

**M0 spikes** (pass/fail):
1. A zbus StatusNotifierHost running next to plasmashell receives items and menus.
2. A compiled C++ QML plugin loads in a Plasma 6 system-tray applet and speaks the protocol
   over the Unix socket.
3. Topology on a real managed Windows laptop with WSL: inbound tailnet TCP to a non-admin
   process, non-admin `whois`, WSL↔Windows forwarding, and which node identity each WSL
   instance has.
4. Windows 11: does notification-icon Demote persist, and are new icons visible by default?
5. Hook payloads for both tools: which notification types actually fire, plus kill and suspend
   behaviour.
6. Bracketed-paste delivery into the Claude Code and Codex TUIs under tmux.
7. `traytrayd` cross-builds for `x86_64-pc-windows-gnu` and runs on Windows 11.

**Alpha-a:** core, proto, the Rust SDK, the conformance suite, the KDE shell, and the agents app
running locally.

**Alpha-b:** the .NET SDK, the Windows shell, robo-rightclick 1.1 integration, and an ephemeral
audit on Windows.

**Alpha-c:** cross-machine pairing and remote agents.

**Beta:** tier-2 web panels, the drawer if it slipped, a Python SDK, more apps.

## Tests that must exist

Each of these is run once with its check removed and seen to fail.

- **Conformance:**
  - a stale `rev` is dropped
  - `event_id` is deduped
  - an app can't write another app's state or send host-to-app frames
- **Pairing:**
  - lockout after 3 wrong codes
  - TTL
  - a code entered from another node
  - a concurrent request
  - a request outside the window
- **Remote:**
  - a token replayed from another node gets `node_mismatch`
  - with the tailnet down at startup there is no listener; the listener appears once the
    tailnet does
- **Reply:**
  - a host not on the allowlist is refused
  - a reused pane is refused
  - no reply is possible during a permission prompt
- **Ephemeral audit:** the app's snapshot set plus the core's and shells' state, log and crash
  directories, compared byte for byte. Includes one sabotage run that the audit must catch.
- **Lifecycle:**
  - sleep/wake
  - killing the core
  - a 6-agent workflow produces zero toasts

## Non-goals (v0)

- a phone client
- a central hub
- multi-user hosts
- an app store or manifest signing
- the host running app-supplied commands
- live icon hosting on Windows
