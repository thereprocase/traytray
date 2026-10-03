# CLAUDE.md — agent operating instructions

Traytray is a per-desktop tray host with an API. A Rust core (`traytrayd`) holds state,
pairing and the feed. Native shells draw it: a Plasma 6 system-tray applet on KDE and a
WinForms tray on Windows. Apps publish into it over one protocol. The design and milestones
are in docs/design.md.

You build on Linux. Windows and the Plasma shell are verified only by running them.

## This repository is public

- Commit as the configured identity (`repro <repro@local>`). No AI attribution and no
  personal names in commits, branches, changelogs or docs.
- Never commit hostnames, tailnet names or addresses, user names, VM paths, keys or ports.
  Describe machines by role ("the Linux desktop", "a Windows laptop with WSL"). Wrappers that
  touch private machines live in `scripts/local/`, which is gitignored.
- README and docs state facts. No slogans or taglines.

## Verification honesty

- "It builds" and "it works" are different claims. Never present the first as the second.
- Runtime claims (Plasma, Windows, tailnet, hooks) cite a dated entry in `docs/testlog.md`.
  docs/testlog.md is append-only.
- A test only ever seen passing proves nothing. For every security or persistence rule, break
  the code on purpose once and confirm the test fails.

## Invariants: never weaken these without an ADR in docs/decisions/

1. **The host never runs app-supplied commands.** Host-local verbs are a closed list
   (`open_url` https-only, `focus_or_launch` for drawer entries with the path taken from the OS,
   never from an app). Everything else is delivered to the owning app.
2. **State documents never touch disk.** The core writes exactly three things: the feed of
   non-ephemeral events, pairing records (token hashes only), and settings. Ephemeral and
   private flags are honoured per item, per event and per connection.
3. **The remote listener binds only to the tailnet address.** No fallback listener, ever.
   Every remote connection needs a token and a matching tailnet identity.
4. **Only shell connections may send actions, replies or dismisses to apps.** An app can write
   only its own state and events.
5. **Reply safety lives in the app, not the host** (see docs/design.md, "Reply safety").

## Code standards

- Clarity over cleverness. Comments explain *why*. No jokes in code, comments or names.
- Conventional commits, each leaving `./scripts/build.sh` and `./scripts/test.sh` green.
- Dependencies are pinned exactly in Cargo.lock and built with `--locked`. Every dependency
  change is its own commit that names the crate, version, publish date and any build script.
  No crate is added without the owner's approval.
