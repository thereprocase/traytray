# M0.5 and M0.6: agent hook payloads and bracketed-paste delivery

Date: 2026-10-03

## Environment (by role)

- The Linux desktop (CachyOS, KDE Plasma 6), tmux 3.7c, Python 3.14.
- Claude Code 2.1.288, model `haiku` (Haiku 4.5), started with `--permission-mode default`,
  `--strict-mcp-config` (no MCP servers) and `--settings <temp file>`. The user settings file
  was not edited.
- Codex CLI 0.160.0 (standalone musl build), the user's configured model with
  `model_reasoning_effort="low"`. Started with `--disable plugins`,
  `--disable skill_mcp_dependency_install` and every configured MCP server set to
  `enabled=false`, because the configured MCP servers would install packages at launch, which the
  spike rules forbid. These are `-c`/`--disable` flags for one invocation, not
  config edits. Approval and sandbox settings were the user's defaults.
- Everything ran on a private tmux server (`tmux -L traytray-spike`) from a scratch directory
  under `/tmp`. Raw payloads and pane captures are in `spikes/m0/private/` (gitignored).
- Code: `spikes/m0/hooks/log_hook.py` (hook logger, always exits 0, prints nothing),
  `spikes/m0/agents/tt.sh` (private-server tmux wrapper),
  `spikes/m0/agents/paste_reply.sh` (strip + `load-buffer` + `paste-buffer -p -d` + Enter).

## M0.5: Claude Code hooks

Hook config: one command hook per event, `timeout: 2`. Run A used `async: true` only on the
events the hooks docs list as async-capable (PostToolUse, Notification, SessionEnd). Run B used
`async: true` on all eleven events. Neither produced a settings error.

### Payload field names (observed)

| Event | Fields |
|---|---|
| SessionStart | `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `source` (`startup`), `model` (interactive only; absent in `-p`) |
| UserPromptSubmit | `session_id`, `prompt_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`, `prompt` |
| PreToolUse | common + `permission_mode`, `tool_name`, `tool_input`, `tool_use_id` |
| PermissionRequest | common + `permission_mode`, `tool_name`, `tool_input`, `permission_suggestions` (objects with `type`, `destination`, `directories` or `mode`). No `tool_use_id`. |
| PostToolUse | common + `permission_mode`, `tool_name`, `tool_input`, `tool_response`, `tool_use_id`, `duration_ms` |
| Notification | `session_id`, `prompt_id`, `transcript_path`, `cwd`, `hook_event_name`, `notification_type`, `message`. No `permission_mode`. |
| Stop | common + `permission_mode`, `last_assistant_message`, `stop_hook_active` (false), `background_tasks` ([]), `session_crons` ([]) |
| SessionEnd | `session_id`, `prompt_id`, `transcript_path`, `cwd`, `hook_event_name`, `reason` |

"common" = `session_id`, `prompt_id`, `transcript_path`, `cwd`, `hook_event_name`.
`permission_mode` was `"default"` in every payload, although the TUI footer says
"manual mode on" and `claude --help` lists `manual`, not `default`, as a mode.
No payload carried `agent_id` or `agent_type` (no subagents were run).
Example `message` values: `"Claude needs your permission"`, `"Claude is waiting for your input"`.

Hook process environment (names; values only for non-secret ones): `CLAUDECODE=1`,
`CLAUDE_CODE_ENTRYPOINT` (`cli` interactive, `sdk-cli` for `-p`),
`CLAUDE_CODE_SESSION_ATTENDED` (`1` interactive, `0` for `-p`), `CLAUDE_PID` (equal to the
claude process PID), `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_PROJECT_DIR`.
The hook's parent PID was the claude process in every case.

### Results

**1. Permission prompt — PASS.** Both signals fire, ~6 s apart.
Evidence (seconds relative to PreToolUse): PreToolUse 0.00, PermissionRequest +0.24,
Notification `notification_type=permission_prompt` +6.27. Second run: +0.07 and +6.11.
The pane showed `Do you want to proceed? ❯ 1. Yes / 2. Yes, and always allow ... / 3. No`.

**1b. Approve, then PostToolUse and Stop — PASS.** After the Enter that approved:
PostToolUse +0.66 s, Stop +1.72 s. The file was created.

**1c. User denies the prompt (key `3`) — FAIL for the design's assumption.** Pane:
`⎿ Interrupted · What should Claude do instead?`. No PermissionDenied, no PostToolUse, no Stop,
and no Notification `idle_prompt` in the following 124 s. Nothing at all tells the daemon the
prompt is gone. (A feedback survey, `How is Claude doing this session? 1: Bad 2: Fine 3: Good
0: Dismiss`, was on screen during that wait and may have suppressed `idle_prompt`.)
PermissionDenied also did not fire for the `-p` case where nobody can approve (see below).
PermissionDenied was never observed.

**2. Idle >= 70 s — PASS.** Notification `idle_prompt` fired 60.1 s after Stop, once.

**3. `/exit` — PASS.** SessionEnd with `reason=prompt_input_exit`, ~0.5 s after the command.
Extra: killing the tmux pane (SIGHUP) and `kill -TERM` each produced SessionEnd `reason=other`.

**4. `kill -9` — PASS.** Shell reported exit 137; zero hook lines in the following 8 s and
none later. A SIGKILLed session never sends SessionEnd.

**5. `kill -STOP` / `kill -CONT` — PASS (no effect).** Process state went `T` then `R`. No hook
fired. Keys sent while stopped (`xyz`) were not drawn, then appeared in the composer after CONT.

**Headless `claude -p` — PARTIAL.**
- Prompt "reply with the word ok", Run A config: SessionStart, UserPromptSubmit, Stop,
  SessionEnd (`reason=other`).
- Same prompt, Run B config (Stop async): SessionStart, UserPromptSubmit, SessionEnd. **Stop
  missing.** A Bash-tool prompt with Run B config: SessionStart, UserPromptSubmit, PreToolUse,
  PermissionRequest, SessionEnd; the tool was refused (stdout: "The Bash tool needs your
  approval..."), with no PermissionDenied and no Stop. Async Stop was lost in 2 of 2 `-p` runs.
  In interactive sessions async Stop fired every time (7 of 7).
- No payload field marks a run as headless: `permission_mode` is `default` in both and there is
  no `agent_id`. The only payload difference is that SessionStart lacks `model`. The hook
  environment tells them apart: `CLAUDE_CODE_ENTRYPOINT=sdk-cli` and
  `CLAUDE_CODE_SESSION_ATTENDED=0`.

**Not exercised:** StopFailure (no API error happened), SubagentStop (no subagent was started).

## Codex hooks — BLOCKED (needs the user)

Research (online docs for current Codex, and Codex source):
- **Events:** PreToolUse, PermissionRequest, PostToolUse, PreCompact, PostCompact,
  UserPromptSubmit, SubagentStart, SubagentStop, Stop, Interrupt (docs only), SessionStart,
  SessionEnd. There is no Notification or idle event.
- **Where hooks live:** `$CODEX_HOME/hooks.json` (default `~/.codex/hooks.json`) or `[hooks]`
  tables in `config.toml`, plus `<repo>/.codex/hooks.json` / `<repo>/.codex/config.toml`, plus
  plugin manifests. If one layer has both JSON and TOML hooks, Codex warns.
- **Common input fields:** `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`.
  Turn-scoped events add `turn_id` and `permission_mode` (`default`, `acceptEdits`, `plan`,
  `dontAsk`, `bypassPermissions`), and optionally `agent_id` / `agent_type` for subagents.
  UserPromptSubmit adds `prompt`. Tool events add `tool_name`, `tool_input` and `tool_use_id`
  (PermissionRequest has no `tool_use_id`). PostToolUse adds `tool_response`. Stop adds
  `stop_hook_active` and `last_assistant_message`. SessionStart adds `source`. SessionEnd has
  `reason`, which is always `other`.
- **Timeouts and async:** 600 s default; SessionEnd (and Interrupt) default 1 s, capped at 3 s.
  The docs say `async: true` is supported (up to 8 concurrent per session). The 0.147 source
  instead skipped async hooks with a warning. Not verified on 0.160.
- **Trust:** a non-managed hook runs only if `config.toml` contains a
  `hooks.state."<key>".trusted_hash` equal to the hash of the hook's normalized definition.
  `<key>` is `<source file>:<event label>:<group index>:<handler index>`. Trust is granted in the
  TUI: a startup dialog, or `/hooks`. Any edit to a hook changes its hash and requires review
  again. Managed (system/MDM) hooks skip this. A `--dangerously-bypass-hook-trust` flag exists;
  it was not used.

Observation: two hooks (UserPromptSubmit, Stop) were passed as `-c hooks.*` overrides for one
invocation. Codex stopped at a dialog: `Hooks need review / 2 hooks are new or changed. / Hooks
can run outside the sandbox after you trust them. / 1. Review hooks / 2. Trust all and continue /
3. Continue without trusting (hooks won't run)`. Option 3 was chosen. After 9 turns there were
zero hook log lines. The SHA-256 of `config.toml` and its mtime were unchanged. The trust gate
works and nothing was trusted.

**What the user would have to approve:** the traytray hook entries (a `hooks.json` in
`$CODEX_HOME`, or a `[hooks]` block) and then, in a Codex TUI, `Review hooks` → trust. That
writes one `hooks.state."<key>".trusted_hash` entry per hook into `$CODEX_HOME/config.toml`.

**Trust-free signal (observed, for information):** the legacy `notify` program (`-c notify=[...]`)
ran without any trust step. It gets one JSON argument per completed turn:
`type` (`agent-turn-complete`), `thread-id`, `turn-id`, `cwd`, `client` (`codex-tui`),
`input-messages` (all user messages of the thread so far), `last-assistant-message`.
It also fired for an internal title-generation turn on a different `thread-id`, which a daemon
must filter out. Its parent PID was the codex process.

**Process model caveat:** with any `-c`/`--enable`/`--disable` flag, Codex warned
`Running without the shared background server: command-line configuration overrides ... requires
embedded mode`. With the user's plain default config the TUI may attach to a shared app-server
daemon. Then hook and notify processes may not be children of the pane's process. This was not
tested, because the plain default config would have launched configured MCP servers.

## M0.6: bracketed-paste delivery

Method: `tmux load-buffer -b <name> -` (from the stripper) or `load-buffer <file>` (raw), then
`tmux paste-buffer -p -d -b <name> -t <pane>`, 0.3 s pause, `tmux send-keys -t <pane> Enter`.
Claude delivery was checked against the UserPromptSubmit `prompt` field (byte comparison) and the
pane. Codex delivery was checked against `notify` `input-messages` and the pane, because its hooks
were untrusted.

| Text | Claude Code | Codex |
|---|---|---|
| Plain sentence | PASS: one prompt, byte-exact | PASS: one turn, exact |
| `<tag attr="x">`, backticks, `<<EOF>>` | PASS: byte-exact | PASS: exact |
| 3 lines with `\n` | PASS: one prompt, `\n` preserved | PASS: one turn, `\n` preserved |
| ESC and `ESC[201~`, no app strip, tmux default | arrives as literal `^[` and `^[[201~` text | same |
| same, tmux sanitizing off (`paste-buffer -S`) | ESC+`a` eaten as Alt-a (`a` lost); `ESC[201~` eaten; rest typed | ESC eaten, rest typed |
| `ok.ESC[201~\rReply with the word second.`, `-S` | the scripted Enter was swallowed: text sat unsent in the composer, and a second Enter sent it as one prompt with `\n` | **split into two submitted user messages** (`input-messages` count 6→8 in one turn; the agent answered "second") |
| both ESC texts after app strip (C0 except `\n`/`\t`, DEL, C1 removed; CR → LF) | PASS: one prompt each | PASS: one turn each |

Result: **PASS.** Bracketed paste plus Enter delivers each test text as one message to both
TUIs. Stripping is required. tmux 3.7c sanitizes control bytes in `paste-buffer` by default
(vis(3); `-S` disables it). With that safety removed, an embedded `ESC[201~` plus CR injects a
second prompt into Codex and leaves Claude Code's composer holding unsent text.

### pane_pid / pane_current_command

| How the agent was started | pane_pid | pane_current_command |
|---|---|---|
| claude as the pane's command | the claude PID | `claude` |
| claude typed into an interactive bash | the bash PID | `claude` (back to `bash` after `/exit`) |
| codex typed into an interactive bash | the bash PID | `codex` |
| claude inside `bash -c "claude ...; echo; sleep"` | the bash PID | `bash` |

`claude` is a symlink to a versioned binary. The process name is the name it was exec'd under,
so running the versioned path would show the version string instead of `claude`. Codex is a
static ELF named `codex`.

## Consequences for docs/design.md

1. **Claude Code events.** Add PermissionRequest as the needs_you trigger. It fires right away;
   Notification `permission_prompt` comes ~6 s later. Keep `permission_prompt` as a backup, and
   dedupe both into one waiting episode.
2. **Clearing "waiting".** "PostToolUse (clears waiting)" is not enough. A user "No" produces no
   event at all: no PermissionDenied, no Stop, and no `idle_prompt` seen within 2 min.
   PermissionDenied never fired in any scenario here. Clear waiting on any later event from the
   session (UserPromptSubmit, PreToolUse, PostToolUse, Stop, SessionEnd). Add a pane re-read (is
   the permission dialog still on screen?) or a timeout so a denied prompt doesn't stay
   needs_you. Drop PermissionDenied from the list of events the design relies on, or mark it
   classifier-only and unverified.
3. **Headless detection.** No payload field marks a `-p` run. The hook script must forward
   `CLAUDE_CODE_ENTRYPOINT` and `CLAUDE_CODE_SESSION_ATTENDED` from its environment. "Headless
   `-p` runs ... one quiet row" depends on this.
4. **Async.** "Runs async" loses Stop in `-p` runs (2/2). Recommend `async: true` only on
   Notification, PostToolUse and SessionEnd (the documented set), and synchronous `timeout: 2`
   elsewhere. The script only writes one line to a socket and fails fast when the daemon is down.
5. **Liveness.** Record `CLAUDE_PID` (= the hook's parent PID) at SessionStart. Confirmed: SIGKILL
   gives no SessionEnd; SIGHUP (pane closed) and SIGTERM give SessionEnd `reason=other`. A
   SIGSTOPped session fires nothing and looks alive; the daemon could show it as suspended by
   reading the process state.
6. **Pane check (Reply safety 2).** Comparing `pane_current_command` alone is weak. It reads
   `bash` under a `bash -c` wrapper, and a versioned exec name changes it. Compare the recorded
   agent PID with the pane's foreground process group (or check the PID is a descendant of
   `pane_pid`), and keep the command name as a secondary check.
7. **Delivery (Reply safety 5).** Keep the app-side strip and never pass `paste-buffer -S`. Also
   normalize CR to LF before stripping: a CR after an early paste close is the injection path. Do
   not rely on tmux's vis(3) sanitizing alone: it turns ESC into literal `^[` text and older tmux
   versions may not do it (the version that introduced it was not checked).
8. **Dialogs (Reply safety 3).** Add Claude Code's session feedback survey (`1: Bad 2: Fine
   3: Good 0: Dismiss`, seen after an interrupted turn) to the "no reply box" states. A reply that
   starts with a digit could answer it. A digit-free reply dismissed it and was delivered normally.
9. **Tile `permission_mode`.** Hooks report `default` while the UI says "manual". Map it for
   display.
10. **Codex.** Hook trust is a user step: the onboarding text should say exactly what to approve
    (see above). Codex has no Notification/idle hook, so the pane-footer fallback stays. `notify`
    is a trust-free turn-complete signal, but it fires for internal title-generation turns and
    should not count as hook trust. SessionEnd `reason` is always `other`. Whether hook processes
    are children of the pane's codex process under the shared app-server daemon is untested; until
    it is, the pane check for Codex should not assume it.
11. **Starting tmux servers.** A user's tmux config can run hooks (for example a session restore)
    in any new tmux server, including a private `-L` one. The agents app must never start a tmux
    server. Test harnesses should use `tmux -f /dev/null -L ...`.

## Cleanup done

- Every claude and codex process started here was ended: by `/exit`, SIGKILL (test 4), SIGHUP
  and SIGTERM (extra tests), or by `kill-server` on the private tmux server. A `pgrep` check
  afterwards found no leftover agent, hook or MCP process. No package-install process ever ran.
- The private tmux server was killed and its stale socket file removed. The scratch directory
  under `/tmp` was removed.
- Side effects outside the spike dirs, left in place and reported:
  - The user's tmux config hooks ran inside the private server; the restored sessions were
    killed, and the user's own saved tmux state was checked afterwards and is unchanged.
  - Claude Code recorded workspace trust for the scratch directory in its user state file and
    kept transcripts under its per-project directory. Codex kept its session records. Codex
    `config.toml` is unchanged (same hash).
- Nothing was committed or pushed.

## Reviewer corrections (independent check, same day)

- Test 5 (STOP/CONT) has no retained evidence and was close to vacuous (idle session). Treat it
  as NOT TESTED.
- Test 4 (kill -9): consistent with the hook log (SessionStart with no SessionEnd); the exit code
  was not retained.
- Async-Stop loss rests on one clean A/B pair, and the "7 of 7 interactive" figure cannot be tied
  to a known config. Treat "async Stop is unreliable in headless runs" as LIKELY, not VERIFIED.
- The pane_current_command table has no retained capture. Recommendation 6 (compare PIDs, not
  command names) stands on its own merits.
- The deny result was seen once, with the feedback survey on screen. "Nothing signals a denied
  prompt" is LIKELY, not VERIFIED.
- CLAUDE_CODE_CHILD_SESSION=1 appeared in every hook environment; the agents under test may have
  run as child sessions of the spike agent, so survey/idle behaviour may differ for a normal user
  session. POSSIBLE.
- The harness has since been fixed to start tmux with `-f /dev/null`.
