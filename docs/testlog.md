# Test log

Append-only. Each entry: date, machine role and OS build, what ran, what was observed.
Runtime claims elsewhere in the repo cite an entry here.

## 2026-10-03 · M0 spikes (Linux desktop, Windows 11 VM)

Details and reviewer corrections: spikes/m0/results/{kde,agents,windows}.md.

- **Linux desktop, KDE Plasma 6.7.4 Wayland, Qt 6.11.2.**
  - M0.1 PASS: a zbus StatusNotifierHost registered next to plasmashell; plasmashell's tray kept
    working; one real item and a test item were read live (properties + dbusmenu GetLayout);
    Registered/NewIcon/NewStatus/Unregistered signals arrived (from the test item only).
  - M0.2 PASS in plasmawindowed: a compiled C++ QML plugin (QLocalSocket, NDJSON) loaded, showed
    frames, sent frames back, and rejected lines over 256 KB while continuing. In the real system
    tray: PARTIAL — not tried (adding to the tray was out of scope). plasmashell's QML engine
    has no extra import paths, so the plugin must be bundled in the plasmoid package (LIKELY).
- **Linux desktop, Claude Code 2.1.288 and Codex CLI 0.160.0, tmux 3.7c.**
  - M0.5 Claude: PermissionRequest and Notification(permission_prompt) fire on a permission
    prompt; PostToolUse then Stop after approval; idle_prompt ~60 s after Stop; SessionEnd on
    /exit, pane close and SIGTERM; none on kill -9. A denied prompt produced no further hook
    (seen once). Headless runs are told apart only by hook environment variables.
  - M0.5 Codex: BLOCKED — hooks need the user's trust; none ran.
  - M0.6 PASS: bracketed paste via load-buffer + paste-buffer -p delivered plain, bracket/backtick
    and multi-line texts as one message to both TUIs; unstripped ESC[201~ broke Codex into two
    prompts, stripped text did not.
- **Windows 11 VM, build 26200.6584.**
  - M0.7 PASS: a std-only Rust exe cross-built for x86_64-pc-windows-gnu ran a named-pipe server
    with a current-user DACL, read the client PID, exchanged one NDJSON frame each way; SYSTEM was
    refused; a same-user ssh (network logon) client connected until NETWORK was denied.
  - M0.4 PASS: new tray icons land in the overflow; a NotifyIconSettings key appears per exe
    path; IsPromoted 1/0 applies live and survives an Explorer kill and a sign-out/sign-in.
- **M0.3 (managed laptop topology): not run** — waiting for the owner's go-ahead.
