#!/usr/bin/env python3
"""Spike M0.5 hook logger.

Appends one JSON line per hook invocation: the event name passed on argv, a host
timestamp, the hook's own pid and parent pid (to learn which process a liveness check
would have to watch), and the raw stdin payload. Always exits 0 and prints nothing, so
it can never steer or block the agent under test.
"""

import json
import os
import sys
import time

DEFAULT_LOG = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "..", "private", "hook-events.jsonl"
)

SAFE_ENV_VALUES = (
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_PID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_CHILD_SESSION",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
)


def main() -> None:
    event_arg = sys.argv[1] if len(sys.argv) > 1 else ""
    log_path = os.environ.get("TRAYTRAY_SPIKE_HOOK_LOG", DEFAULT_LOG)
    raw = ""
    try:
        raw = sys.stdin.read()
        payload = json.loads(raw) if raw.strip() else None
    except (
        ValueError,
        OSError,
    ):  # keep the raw text when stdin is not JSON (e.g. Codex notify argv)
        payload = None
    record = {
        "ts": time.time(),
        "event_arg": event_arg,
        "hook_pid": os.getpid(),
        "hook_ppid": os.getppid(),
        "argv_extra": sys.argv[2:],
        # Names only for agent variables (some hold tokens); values only for a safe allowlist.
        "agent_env_names": sorted(
            k for k in os.environ if k.startswith(("CLAUDE", "CODEX"))
        ),
        "agent_env_values": {
            k: os.environ[k] for k in SAFE_ENV_VALUES if k in os.environ
        },
        "payload": payload,
        "raw_if_not_json": raw if payload is None else None,
    }
    try:
        with open(log_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(record) + "\n")
    except OSError:
        pass


if __name__ == "__main__":
    try:
        main()
    finally:
        sys.exit(0)
