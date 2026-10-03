#!/usr/bin/env bash
# Run tmux against the spike's private server only, with the calling agent's own
# session variables removed so the agents under test do not think they are nested.
exec env -u CLAUDECODE -u CLAUDE_CODE_BRIDGE_SESSION_ID -u CLAUDE_CODE_CHILD_SESSION \
  -u CLAUDE_CODE_ENTRYPOINT -u CLAUDE_CODE_EXECPATH -u CLAUDE_CODE_MESSAGING_SOCKET \
  -u CLAUDE_CODE_MESSAGING_TOKEN -u CLAUDE_CODE_SESSION_ATTENDED -u CLAUDE_CODE_SESSION_ID \
  -u CLAUDE_EFFORT -u CLAUDE_PID -u TMUX -u TMUX_PANE \
  tmux -f /dev/null -L traytray-spike "$@"
