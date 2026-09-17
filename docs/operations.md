# Running WhatsAI

## Local daemon

`whatsai start --name Alice` starts a daemon using `WHATSAI_STATE` (default `~/.local/share/whatsai`). `--name` only matters for a first start that creates the identity; it defaults to `WHATSAI_NAME`, then your account name. Repeated or concurrent starts are idempotent and return the running daemon's health. The Claude plugin's `SessionStart` hook and the MCP adapter call `whatsai start` for you. `whatsai health` checks it and `whatsai stop` shuts it down. Logs are in the state directory. Private state and IPC are restricted to the local user; each state directory has a single-process lock. The daemon socket lives inside the state directory, and Unix socket paths are limited to about 100 bytes, so keep `WHATSAI_STATE` short; the daemon and CLI refuse an overlong path with an explicit error rather than failing at bind time. A foreground alternative is `whatsai-daemon --state /absolute/state --name Alice`.

For direct connections plus relay fallback, set `WHATSAI_RELAY=https://relay.example.net` in the daemon environment. No relay or public discovery is configured by default. `WHATSAI_RELAY=default` explicitly opts into iroh's default relay servers. Service and relay are different endpoints: the HTTPS authority handles admission/current authorization and ciphertext storage, while the iroh relay forwards live encrypted connections.

## User services

Render a definition without installing it:

```sh
python3 scripts/install-user-service.py --platform linux --binary /absolute/bin/whatsai-daemon --state /absolute/state --name Alice --output /tmp/whatsai.service
python3 scripts/install-user-service.py --platform macos --binary /absolute/bin/whatsai-daemon --state /absolute/state --name Alice --output /tmp/local.whatsai.daemon.plist
```

On Linux, place the reviewed unit in `~/.config/systemd/user/whatsai.service`, add `Environment=WHATSAI_RELAY=https://relay.example.net` under `[Service]` when using a relay, then run `systemctl --user daemon-reload` and `systemctl --user enable --now whatsai`. Diagnose with `systemctl --user status whatsai` and `journalctl --user -u whatsai`.

On macOS, place the plist in `~/Library/LaunchAgents/local.whatsai.daemon.plist` and load it with `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/local.whatsai.daemon.plist`. Add relay configuration via the plist's `EnvironmentVariables` dictionary. Plist generation is tested on Linux; macOS runtime loading remains an acceptance gate.

To uninstall, stop/disable the user service, remove its service definition, and remove the plugin/MCP registration through the harness's normal UI/CLI. Keep the state directory unless you deliberately want to lose that identity and its local inbox. Do not delete the Git checkout or harness credentials. Removing a member from a team does not revoke their separately held Git hosting access.

## Self-hosted authority and relay

Run `whatsai-service --state /srv/whatsai --listen 127.0.0.1:8787 --quota-bytes 1073741824` under a service supervisor. The state directory must be owned by that service user. The quota covers serialized encrypted event/chunk content. Messages and chunks can be denied at capacity; the client must not claim complete storage then.

Terminate HTTPS in front of the loopback service; `deploy/Caddyfile` provides a minimal Caddy configuration using the `WHATSAI_DOMAIN` environment variable. Point DNS at the host before starting Caddy. The application rejects non-loopback plain HTTP authority URLs. `/health` returns the protocol version. Request identities are authenticated using signatures; this early service has no billing/account system. Apply operator ingress controls and storage limits before any shared deployment.

Run the upstream iroh relay independently, pinned to the tested version:

```sh
cargo install iroh-relay --version 1.2.0 --features server --locked
```

`deploy/iroh-relay.toml` is a loopback-only development configuration. For a publicly reachable relay, configure its TLS certificate and HTTPS bind address using the [iroh relay server configuration](https://github.com/n0-computer/iroh/tree/v1.2.0/iroh-relay). Do not expose the loopback HTTP example unchanged. Configure clients with the public HTTPS relay URL. The application transport tests launch an embedded self-hosted relay and disable IP transport to prove relay traversal; that is not a public TLS deployment test.

No public hosting was deployed during implementation. Review the project's license before distributing a release.

## Persistence, upgrades, and recovery

Back up client identity.json and client.db together; back up service.db for the authority. Stop the relevant process before copying its complete state directory so SQLite WAL files and keys remain consistent. Protect backups as private data. Restoring a mailbox without its identity keys does not restore decryption access. A missing identity beside an existing client database causes an error rather than silent key replacement.

The database and wire protocol are versioned. Unknown newer database versions are refused. Before upgrading, stop workers and keep a matching state backup. Roll back using the previous binary and its matching state snapshot; no schema downgrade is promised. Never restore the same identity on two running machines in this MVP.

Service-stored content is authorized for seven days; expiry does not imply secure erasure from backups. Expired payload cleanup/compaction is not yet automated. Plan storage monitoring and offline maintenance rather than assuming quotas recycle automatically.

## Troubleshooting

- **Daemon unavailable:** check `WHATSAI_STATE`, run `whatsai start`, inspect daemon.log. Do not start multiple daemons over one database.
- **Join pending:** an existing admin must approve the displayed fingerprint with `whatsai approve`. The creator need not be online if another admin exists.
- **Membership changed while queued:** the outbox reports a failure; explicitly resend after reviewing the current roster. WhatsAI does not silently share old queued content with newly added members.
- **Service unavailable:** queued messages remain local and existing inbox content is readable; new direct transfers also require current authority authorization.
- **Worker authentication error:** authenticate the selected harness locally. The daemon does not receive or repair your model account credentials.
- **Worker interrupted/failed:** inspect worker status and inbox dispatch state. Uncertain model work is not automatically replayed; send a new explicit request when appropriate.
- **File partial:** retry the same download to reuse verified chunks. Choose a different directory if the destination already exists.
- **Handoff fails:** confirm the local origin exactly matches the team's credential-free remote, Git authentication works, and the sender pushed the exact commit. Receipt itself never runs Git.
