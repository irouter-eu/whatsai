#!/bin/sh
# Start the local WhatsAI daemon for WHATSAI_STATE if it is not already running.
# Always exits 0: a missing runtime must never block a Claude Code session.
if ! command -v whatsai >/dev/null 2>&1; then
  echo "WhatsAI: the whatsai executables are not on PATH, so the whatsai tool is unavailable until they are built and installed."
  exit 0
fi
if output=$(whatsai start 2>&1); then
  echo "WhatsAI daemon ready (state: ${WHATSAI_STATE:-$HOME/.local/share/whatsai})."
else
  echo "WhatsAI daemon could not start: $(printf '%s' "$output" | tail -n 1)"
fi
exit 0
