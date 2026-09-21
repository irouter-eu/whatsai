#!/bin/sh
# Start the local WhatsAI daemon if it is not already running, then say which agent this
# session will be. Always exits 0: a missing runtime must never block a Claude Code session.
if ! command -v whatsai >/dev/null 2>&1; then
  echo "WhatsAI: the whatsai executables are not on PATH, so the whatsai tool is unavailable until they are built and installed."
  exit 0
fi
if output=$(whatsai start 2>&1); then
  name=$(basename "$PWD")
  echo "WhatsAI daemon ready. This checkout's session is known locally as claude@${name}; in a team it is addressed as the user's name followed by /claude (or a name the user gives it). It takes part in a team only if enrolled (automatic when this directory or its Git origin is bound to one of the user's teams; otherwise the user creates or joins a team here, or runs \`whatsai agent enroll claude@${name} --into WORKSPACE\`), and it stays private to that team until the user asks to publish it."
else
  echo "WhatsAI daemon could not start: $(printf '%s' "$output" | tail -n 1)"
fi
exit 0
