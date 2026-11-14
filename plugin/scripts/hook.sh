#!/usr/bin/env bash
# Every hook of the plugin comes here: find the hookwatch command and hand it
# the event. The plugin cannot ship the Rust binary, so when it (or gitleaks,
# which masks credentials at Stop) is missing, this passes the event through
# and, at session start, tells the user what to install. Never blocks.
set -uo pipefail
input=$(cat)
bin=$(command -v hookwatch || true)
if [ -z "$bin" ] && [ -x "$HOME/.cargo/bin/hookwatch" ]; then
  bin="$HOME/.cargo/bin/hookwatch"
fi
missing=()
[ -n "$bin" ] || missing+=("the hookwatch command (in the hookwatch repo: just install)")
command -v gitleaks >/dev/null || missing+=("gitleaks (masks credentials in the ledger)")
if [ ${#missing[@]} -gt 0 ]; then
  case "$input" in
    *'"hook_event_name":"SessionStart"'* | *'"hook_event_name": "SessionStart"'*)
      msg="hookwatch plugin is on but not recording: missing $(IFS=';'; echo "${missing[*]}")"
      printf '{"systemMessage":%s}\n' "$(printf '%s' "$msg" | sed 's/\\/\\\\/g; s/"/\\"/g; s/^/"/; s/$/"/')"
      ;;
  esac
  exit 0
fi
printf '%s' "$input" | "$bin" hook
