# Running WhatsAI

## Local daemon

`whatsai start --name Alice` starts a daemon using `WHATSAI_STATE` (default `~/.local/share/whatsai`): one identity per person per machine, shared by every harness and the CLI. Agents (`harness@workspace`) and their sessions live inside it; `whatsai agents` lists them and `whatsai health` shows the directory in use. Release 0.2.0 briefly used `~/.local/share/whatsai/<harness>` directories; if you have one, stop its daemon and move its contents back to `~/.local/share/whatsai`, keeping the identity that founded or joined your team. The client database upgrades itself on first open; older binaries refuse the newer schema. `--name` only matters for a first start that creates the identity; it defaults to `WHATSAI_NAME`, then your account name. Repeated or concurrent starts are idempotent and return the running daemon's health. The Claude plugin's `SessionStart` hook and the MCP adapter call `whatsai start` for you. `whatsai health` checks it and `whatsai stop` shuts it down. Logs are in the state directory. Private state and IPC are restricted to the local user; each state directory has a single-process lock. The daemon socket lives inside the state directory, and Unix socket paths are limited to about 100 bytes, so keep `WHATSAI_STATE` short; the daemon and CLI refuse an overlong path with an explicit error rather than failing at bind time. A foreground alternative is `whatsai-daemon --state /absolute/state --name Alice`.

Each daemon binds one iroh QUIC endpoint and remembers its UDP port in the state directory, so a restarted daemon keeps the direct address teammates already know; if that port is taken it binds a new one and says so in the log. The daemon also embeds the team authority: for a team it founded, it serves admission, the signed membership history and the encrypted mailbox to other members over the same iroh endpoint, from `<state>/authority/service.db`. `WHATSAI_QUOTA_BYTES` caps that mailbox (default 1 GiB); at capacity, puts fail explicitly.

## User services

Render a definition without installing it:

```sh
python3 scripts/install-user-service.py --platform linux --binary /absolute/bin/whatsai-daemon --state /absolute/state --name Alice --output /tmp/whatsai.service
python3 scripts/install-user-service.py --platform macos --binary /absolute/bin/whatsai-daemon --state /absolute/state --name Alice --output /tmp/local.whatsai.daemon.plist
```

On Linux, place the reviewed unit in `~/.config/systemd/user/whatsai.service`, add `Environment=WHATSAI_RELAY=https://relay.example.net` under `[Service]` when using a relay, then run `systemctl --user daemon-reload` and `systemctl --user enable --now whatsai`. Diagnose with `systemctl --user status whatsai` and `journalctl --user -u whatsai`.

On macOS, place the plist in `~/Library/LaunchAgents/local.whatsai.daemon.plist` and load it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.whatsai.daemon.plist`. Add relay configuration via the plist's `EnvironmentVariables` dictionary. Plist generation is tested on Linux; macOS runtime loading remains an acceptance gate.

To uninstall, stop/disable the user service, remove its service definition, and remove the plugin/MCP registration through the harness's normal UI/CLI. Keep the state directory unless you deliberately want to lose that identity and its local inbox. Do not delete the Git checkout or harness credentials. Removing a member from a team does not revoke their separately held Git hosting access.

## Reachability and relays

There is no authority server to host. What a team needs is for members' daemons to reach each other, and for the founder's daemon to be reachable when someone joins or catches up on the mailbox.

By default (`WHATSAI_RELAY` unset or `default`) daemons use iroh's public relay servers: connections start through the relay and upgrade to a direct path when NAT traversal succeeds. Relays only ever carry encrypted QUIC. `whatsai health` reports `last_peer_path` and `last_file_path` as `direct` or `relay`. `whatsai invite` reports `relay: true` once the founder's key includes a relay address; keys generated before that only work on the local network.

`WHATSAI_RELAY=off` disables relays for LAN-only or air-gapped teams; every member must then reach the others' direct addresses, and remembered UDP ports keep those addresses stable across restarts.

To keep all traffic on infrastructure you control, run the upstream iroh relay pinned to the tested version and point every daemon at it with `WHATSAI_RELAY=https://relay.example.net`:

```sh
cargo install iroh-relay --version 1.2.0 --features server --locked
```

`deploy/iroh-relay.toml` is a loopback-only development configuration. For a publicly reachable relay, configure its TLS certificate and HTTPS bind address using the [iroh relay server configuration](https://github.com/n0-computer/iroh/tree/v1.2.0/iroh-relay). Do not expose the loopback HTTP example unchanged. The transport tests launch an embedded self-hosted relay and disable IP transport to prove relay traversal; that is not a public TLS deployment test.

The founder's daemon is the team's only authority in this implementation. Run it as a user service so it stays up: while it is offline, joins, approvals and mailbox delivery wait, and `whatsai sync` reports the authority as unreachable. Members who are both online continue exchanging messages and files directly. Mirroring the authority to every admin's daemon is planned.

## Persistence, upgrades, and recovery

Back up client identity.json and client.db together, and for a founder also `authority/service.db`, which holds the team's membership history, join requests and encrypted mailbox. Stop the relevant process before copying its complete state directory so SQLite WAL files and keys remain consistent. Protect backups as private data. Restoring a mailbox without its identity keys does not restore decryption access. A missing identity beside an existing client database causes an error rather than silent key replacement.

The database and wire protocol are versioned. Unknown newer database versions are refused. Before upgrading, stop workers and keep a matching state backup. Roll back using the previous binary and its matching state snapshot; no schema downgrade is promised. Never restore the same identity on two running machines in this MVP.

Mailbox content is authorized for seven days; expiry does not imply secure erasure from backups. Expired payload cleanup/compaction is not yet automated. Plan storage monitoring and offline maintenance rather than assuming quotas recycle automatically.

## Troubleshooting

- **Daemon unavailable:** check `WHATSAI_STATE`, run `whatsai start`, inspect daemon.log. Do not start multiple daemons over one database.
- **Join pending:** an existing admin must approve the displayed fingerprint with `whatsai approve`. The founder's daemon must be online for the request and the approval to be recorded.
- **Join fails with authority unreachable:** the founder's daemon is offline or the key predates its relay connection. Ask for a fresh `whatsai invite` output showing `relay: true`.
- **Membership changed while queued:** the outbox reports a failure; explicitly resend after reviewing the current roster. WhatsAI does not silently share old queued content with newly added members.
- **Authority unreachable:** queued messages remain local and existing inbox content is readable; new direct transfers also require current authorization from the founder's daemon.
- **A session says its agent is not enrolled:** that workspace is not part of the team. Checkouts of the team repository enroll themselves; anything else needs `whatsai agent enroll LABEL` from your shell, which is deliberate so an unrelated project cannot read team messages or the join key. `whatsai agent unenroll LABEL` closes it again.
- **A teammate cannot address my agent:** it is not published. `whatsai agents` shows `published`; `whatsai agent publish LABEL` makes it visible, and `whatsai agent auto-publish team-repo` does so automatically for checkouts of the team repository only.
- **Messages for an agent never arrive:** run `whatsai agents` and check the label teammates use matches; a retired agent is not offered to the team, and a moved checkout is a new agent until you `whatsai agent adopt` the old label into it. Sessions expire 45 seconds after their harness process stops heartbeating.
- **Worker authentication error:** authenticate the selected harness locally. The daemon does not receive or repair your model account credentials.
- **Worker interrupted/failed:** inspect worker status and inbox dispatch state. Uncertain model work is not automatically replayed; send a new explicit request when appropriate.
- **File partial:** retry the same download to reuse verified chunks. Choose a different directory if the destination already exists.
- **Handoff fails:** confirm the local origin exactly matches the team's credential-free remote, Git authentication works, and the sender pushed the exact commit. Receipt itself never runs Git.
