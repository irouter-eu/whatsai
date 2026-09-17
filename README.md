# WhatsAI

Private teamwork for people using different coding agents. A local daemon provides a shared inbox, encrypted messages and files, explicit work status, and Git commit handoffs. Codex and Claude connect through the same local API. Teams are their own networks: the founder's daemon is the team's authority, members reach each other over iroh QUIC directly or through a relay, and you join with a key. There is no server to host.

This is an early implementation. Local acceptance tests and real-harness smoke tests exist; independent-network acceptance and macOS runtime validation are still required before this is a complete cross-network MVP. See [hosting and operations](docs/operations.md).

## Build

Requires Rust, Node 22+, npm, Python 3 for process-level tests, and Git for handoffs.

```sh
make build
make test
make smoke
```

The executables are `target/debug/whatsai` and `whatsai-daemon`. Put both in the same directory on your PATH. Build and install the Node adapter when using MCP:

```sh
npm --prefix adapters run build
npm install -g ./adapters
```

The repository does not install background services or alter harness configuration automatically.

## Create and join a team — no server

Alice starts her daemon and creates the team. Her daemon becomes the network's authority and prints the join key:

```sh
export WHATSAI_STATE="$PWD/.whatsai/alice"
whatsai start --name Alice
whatsai create --repository https://example.com/team/repo.git
whatsai invite
```

The key is one `whatsai1.` string carrying the team ID, the network secret, Alice's fingerprint, and the address of her daemon. `invite` reports `relay: true` once the daemon has a relay connection; before that the key only reaches Alice on the local network, so wait for it before sending the key to someone elsewhere. Hand the key only to people you want on the team.

Bob starts a daemon with a different state directory, or on another machine, and joins with the key:

```sh
export WHATSAI_STATE="$PWD/.whatsai/bob"
whatsai start --name Bob
whatsai join 'whatsai1.KEY_FROM_ALICE'
whatsai register
```

Joining is pending until an admin approves the exact fingerprint. On Alice's client:

```sh
whatsai requests
whatsai approve BOB_FINGERPRINT
whatsai promote BOB_FINGERPRINT
```

Holding the key is not membership. Requests expire after 24 hours. Admins can `reject`, `demote`, and `revoke`; the last admin cannot leave remaining members without an administrator. Members can `leave`. No election or voting machinery is included.

The founder's daemon is the team's authority in this first implementation: it approves admissions, records membership changes, and holds the encrypted mailbox for members who are offline. While it is offline, new admissions and mailbox delivery wait, and members who are online keep talking to each other directly. Mirroring the authority to every admin's daemon is the next step. Daemons use iroh's public relays by default so teams work across NATs with nothing to host; see [hosting and operations](docs/operations.md) for self-hosted relays and LAN-only setups.

## Chat, files, and code handoffs

```sh
whatsai list
whatsai send 'Can you review the API response?'
whatsai send 'Reply with your expected schema' --to MEMBER_FINGERPRINT
whatsai inbox
whatsai outbox
whatsai share ./design.pdf
whatsai files
whatsai download FILE_EVENT_ID --directory ./downloads
whatsai status --set blocked --description 'Waiting for API review'
whatsai handoff --branch feature/api --commit FULL_COMMIT_HASH --description 'Ready for review'
whatsai accept-handoff HANDOFF_EVENT_ID --repo ./existing-checkout --directory ./review-worktree
```

The daemon syncs periodically; `whatsai sync` triggers it explicitly. A locally queued message is not yet stored remotely. Outbox receipts distinguish remote storage from recipient persistence. Agent processing and replies are separate. A file is remotely available only after every chunk is uploaded; files are limited to 32 MiB. Downloads preserve verified chunks across restarts and refuse overwriting existing files.

Code moves through the existing Git remote. An explicit handoff acceptance fetches the exact commit and creates a detached review worktree; it does not change your existing working tree or run the code. The local `origin` must exactly match the team remote. Git credentials remain local.

## Codex and Claude

The [plugin bundle](plugins/whatsai) contains manifests for both harnesses and an MCP definition using `whatsai-mcp`. The MCP process inherits `WHATSAI_STATE`; set it before launching your harness. Agent-created content is always labelled agent through MCP.

This repository is also a plugin marketplace for both harnesses. Installing the plugin registers the skills and the MCP server; it does not build or install the daemon, so complete the build and `npm install -g ./adapters` steps above first so that `whatsai-mcp` and the three executables are on your PATH.

Claude Code:

```sh
claude plugin marketplace add irouter-eu/whatsai
claude plugin install whatsai@whatsai
```

Codex:

```sh
codex plugin marketplace add irouter-eu/whatsai
codex plugin add whatsai@whatsai
```

Alternatively, for MCP-only access in Codex without installing the plugin or its skills:

```sh
codex mcp add whatsai -- whatsai-mcp
```

Then ask Codex to use the WhatsAI tool. The plugin manifest/skills are supplied for plugin installation; do not assume Claude slash-command syntax works unchanged in Codex.

For a local Claude plugin session:

```sh
claude --plugin-dir ./plugins/whatsai
```

The plugin supplies skills such as `/whatsai:create`, `/whatsai:join`, `/whatsai:requests`, `/whatsai:approve`, `/whatsai:send`, and `/whatsai:inbox`. Plugin loading depends on the installed harness's plugin support; the CLI is the stable fallback.

You do not need to start the daemon by hand for a harness session. The Claude plugin runs `whatsai start` from a `SessionStart` hook, and the `whatsai-mcp` adapter starts the daemon on demand when a tool call finds it unavailable, so the first MCP call from any harness also works. Both paths are idempotent and never block the session when the executables are missing; the hook then reports that instead. A first launch names the new identity from `WHATSAI_NAME`, else your account name; the name cannot be changed later, so set `WHATSAI_NAME` before the first start if you want something else. Set `WHATSAI_STATE` in the environment that launches your harness so the hook, the MCP process, and your shell all use the same daemon.

### ChatGPT

Where workspace marketplace import is available, an admin can open **Admin > Plugins > Add > Import marketplace** and enter `https://github.com/irouter-eu/whatsai`, leaving Path empty. OpenAI supports this repository's marketplace format, but imported plugins declaring MCP servers are **desktop only**. The local WhatsAI runtime and `whatsai-mcp` must still be installed and available to the desktop app. This path has not yet been tested end to end with WhatsAI. See [OpenAI's marketplace import documentation](https://learn.chatgpt.com/docs/enterprise/plugin-management).

For ChatGPT web, marketplace import alone does not connect the local daemon. Developer mode supports connecting an MCP server through a public HTTPS endpoint or Secure MCP Tunnel; the tunnel can target a local stdio server such as `whatsai-mcp`. WhatsAI currently ships only the stdio adapter, with no bundled tunnel setup or authenticated HTTP endpoint. Availability depends on account and workspace policy. See [OpenAI's MCP connection documentation](https://developers.openai.com/plugins/deploy/connect-chatgpt).

### Optional automatic replies

Inbound messages are inbox-only by default. Bind and enable a dedicated worker explicitly:

```sh
whatsai worker bind --harness codex --cwd ./checkout --adapter ./adapters/dist/worker.js
whatsai worker enable
whatsai worker status
whatsai worker pause
```

Use `--harness claude` for Claude. This does not inject messages into an unrelated interactive task. Only addressed messages start automatic turns. Defaults are three replies per conversation root and a 120-second timeout, with durable counters and one turn at a time. Codex uses read-only sandboxing and declines interactive approvals; Claude has tools disabled. The first implementation automates chat, not remote code execution. Existing harness authentication and model usage apply.

`whatsai worker reset ROOT_EVENT_ID` resets an exhausted conversation budget at the local owner's request. Interrupted/failed model turns remain explicit; they are not silently rerun.

Opt-in live-model check (uses your existing logins and can incur model usage):

```sh
make worker-smoke
```

## Trust and limits

Content is encrypted on clients before mailbox storage, using per-recipient HPKE key wrapping and authenticated payload encryption. Administrators sign membership/role changes in a chain rooted in the founder fingerprint. The founder's daemon, as the authority, sees routing metadata, member identities, sizes and timing, and remains trusted for freshness/availability; relays see only encrypted QUIC. This is not an independently audited cryptographic product and does not claim forward secrecy or compromised-device recovery.

The founder's daemon must be reachable for new authorization, even on direct peer transfers. While it is offline, local reading and queueing work; admissions and delivery wait. Revocation prevents new authorization, not access to bytes a member already downloaded. Stored messages/files have a seven-day retention window; capacity errors are explicit. The first implementation supports one team and one device identity per state directory. Keep private state backups: identity recovery and multi-device sync are not implemented.
