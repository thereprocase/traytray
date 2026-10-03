#!/usr/bin/env bash
# Spike M0.6: deliver one reply into an agent pane as a single bracketed paste plus Enter.
#   paste_reply.sh [--raw] [--tmux-unsanitized] <pane> <text-file>
# Without --raw, C0 controls other than newline and tab, DEL and C1 controls are stripped
# first, so the text cannot close the bracketed paste early (ESC[201~) or inject keys.
# --tmux-unsanitized passes -S to paste-buffer, turning off tmux's own vis(3) escaping;
# it exists only to show what an unstripped text does on a tmux without that escaping.
set -euo pipefail
raw=0
paste_flags=(-p -d)
if [[ "${1:-}" == "--raw" ]]; then raw=1; shift; fi
if [[ "${1:-}" == "--tmux-unsanitized" ]]; then paste_flags+=(-S); shift; fi
pane="$1"; src="$2"
tt="$(dirname "$0")/tt.sh"
buf="traytray-reply-$$"
if (( raw )); then
  "$tt" load-buffer -b "$buf" "$src"
else
  python3 -c '
import re, sys
text = open(sys.argv[1], encoding="utf-8", errors="replace", newline="").read()
text = text.replace("\r\n", "\n").replace("\r", "\n")  # CR would submit early in some TUIs
text = re.sub(r"[\x00-\x08\x0b-\x1f\x7f-\x9f]", "", text)
sys.stdout.write(text.rstrip("\n"))
' "$src" | "$tt" load-buffer -b "$buf" -
fi
# -p wraps the paste in ESC[200~ ... ESC[201~ when the application enabled bracketed paste;
# -d deletes the buffer afterwards so reply text does not linger in the tmux server.
"$tt" paste-buffer "${paste_flags[@]}" -b "$buf" -t "$pane"
sleep 0.3
"$tt" send-keys -t "$pane" Enter
