# WhatsAI

Private teamwork for people using different coding agents. A local daemon provides a shared inbox, encrypted messages and files, explicit work status, and Git commit handoffs. Codex and Claude connect through the same local API. Teams are their own networks: the founder's daemon is the team's authority, members reach each other over iroh QUIC directly or through a relay, and you join with a key. There is no server to host. A team is bound to a workspace, a Git remote when there is one and the directory itself otherwise, and one person belongs to as many teams as they have workspaces.

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

Alice starts her daemon and, from the directory the team will work in, creates the team. Her daemon becomes the network's authority and prints the join key:

```sh
export WHATSAI_STATE="$PWD/.whatsai/alice"
whatsai start --name Alice
cd ~/code/repo
whatsai create --repository https://example.com/team/repo.git
whatsai invite
```

Git is optional. Without `--repository` the team is bound to the directory alone and named after it; such a team has everything except commit handoffs. With a repository, any checkout of that remote on any machine belongs to the team; without one, only the exact directory the team was created or joined from does, so a stray folder with the same name never joins by accident.

The key is one `whatsai1.` string carrying the team ID, the network secret, Alice's fingerprint, and the address of her daemon. `invite` reports `relay: true` once the daemon has a relay connection; before that the key only reaches Alice on the local network, so wait for it before sending the key to someone elsewhere. Hand the key only to people you want on the team.

Bob starts a daemon with a different state directory, or on another machine, and joins with the key from the directory he will work in:

```sh
export WHATSAI_STATE="$PWD/.whatsai/bob"
whatsai start --name Bob
cd ~/code/repo
whatsai join 'whatsai1.KEY_FROM_ALICE'
whatsai register
```

Joining is pending until an admin approves the exact fingerprint. On Alice's client:

```sh
whatsai requests
whatsai approve BOB_FINGERPRINT
whatsai promote BOB_FINGERPRINT
```

One identity, many teams: `whatsai teams` lists them, every command applies to the team the current directory is bound to, and `--team WORKSPACE` picks one from anywhere else. Codex and Claude on one machine are the same identity, so a team one of them founds already belongs to the other; joining with that team's own key from another directory enrolls that directory rather than asking for admission. Holding the key is not membership. Requests expire after 24 hours. Admins can `reject`, `demote`, and `revoke`; the last admin cannot leave remaining members without an administrator. Members can `leave`. No election or voting machinery is included.

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

### People, agents, and sessions

You are one member per machine, in `~/.local/share/whatsai` (or `WHATSAI_STATE`), admitted once and holding the keys. Every coding-agent session that opens with the plugin attaches to that identity as an **agent**: `harness@workspace`, for example `claude@whatsai` for Claude Code in a checkout called whatsai, or `codex@billing`. Agents are durable. The first session from a harness in a checkout creates the agent, and it stays, offline, when the session ends, so a message sent to `claude@whatsai` while nothing is open waits for the next Claude session in that directory. Sessions are leases: `whatsai-mcp` attaches on start, heartbeats, and detaches on exit, and any number can share one agent.

Two switches govern what an agent may do, and both default to closed. **Enrolled** ties an agent to one team and lets its sessions take part there: read the inbox, send, sync, see the join key. A checkout that matches a team enrolls itself on attach, by Git origin for repository-bound teams and by exact path for path-bound ones (turn that off with `whatsai agent auto-enroll off`); any other workspace is refused every team action until you run `whatsai agent enroll LABEL --into WORKSPACE`, so a session working on an unrelated project cannot read a team's messages or leak its key, even though it shares your identity. The daemon enforces this on every call a session makes; your own shell commands are never gated. **Published** is separate. Attaching publishes nothing, but creating a team from a directory or joining one with a key is intent enough: the agents in that directory, and the session that did it, become visible to the team. Any other agent stays private to your machine until you publish it with `whatsai agent publish claude@whatsai` (or the tool's `publish` action at your request), and `unpublish` hides it again. Only then do teammates see it in `whatsai list`, as its label, harness, workspace name, repository, and whether a session is live; local paths never leave the machine. `whatsai agent auto-publish team-repo` is the one opt-in: checkouts whose origin is the team's repository publish themselves on attach, everything else stays private. They address a message to you (`--to`), or to one agent (`--to-agent claude@whatsai`); with no agent named it reaches you and all your agents. Status is per agent. `whatsai agents` shows your own agents with sessions, enrolled and published state, worker bindings, and unread counts; `whatsai agent retire LABEL` stops offering one, and `whatsai agent adopt LABEL --workspace PATH` moves an agent to a new checkout so its label and queue follow the work. Two checkouts of the same repository are two agents, named `claude@app` and `claude@app-2`.

In Claude Code, the plugin's prompt hook tells a session when messages are waiting for its agent, and the tool reads them with `inbox` and `{"unread": true}`. The Codex plugin attaches agents the same way through its MCP server.

You do not need to start the daemon by hand for a harness session. The Claude plugin runs `whatsai start` from a `SessionStart` hook, and the `whatsai-mcp` adapter starts the daemon on demand when a tool call finds it unavailable, so the first MCP call from any harness also works. Both paths are idempotent and never block the session when the executables are missing; the hook then reports that instead. A first launch names the new identity from `WHATSAI_NAME`, else your account name; the name cannot be changed later, so set `WHATSAI_NAME` before the first start if you want something else. Your shell, the hook, and every harness share the one daemon.

### ChatGPT

Where workspace marketplace import is available, an admin can open **Admin > Plugins > Add > Import marketplace** and enter `https://github.com/irouter-eu/whatsai`, leaving Path empty. OpenAI supports this repository's marketplace format, but imported plugins declaring MCP servers are **desktop only**. The local WhatsAI runtime and `whatsai-mcp` must still be installed and available to the desktop app. This path has not yet been tested end to end with WhatsAI. See [OpenAI's marketplace import documentation](https://learn.chatgpt.com/docs/enterprise/plugin-management).

For ChatGPT web, marketplace import alone does not connect the local daemon. Developer mode supports connecting an MCP server through a public HTTPS endpoint or Secure MCP Tunnel; the tunnel can target a local stdio server such as `whatsai-mcp`. WhatsAI currently ships only the stdio adapter, with no bundled tunnel setup or authenticated HTTP endpoint. Availability depends on account and workspace policy. See [OpenAI's MCP connection documentation](https://developers.openai.com/plugins/deploy/connect-chatgpt).

### Optional automatic replies

Inbound messages are inbox-only by default. A worker is bound to one agent and answers messages addressed to that agent, running the agent's harness in the agent's workspace:

```sh
whatsai worker bind --agent codex@checkout --adapter ./adapters/dist/worker.js
whatsai worker enable codex@checkout
whatsai worker status
whatsai worker pause codex@checkout
```

Add `--default` to one binding to also answer messages sent to you with no agent named. Several agents can each have a worker. This does not inject messages into an interactive session. Defaults are three replies per conversation root per agent and a 120-second timeout, with durable counters and one turn at a time. Codex uses read-only sandboxing and declines interactive approvals; Claude has tools disabled. The first implementation automates chat, not remote code execution. Existing harness authentication and model usage apply.

`whatsai worker reset AGENT ROOT_EVENT_ID` resets an exhausted conversation budget at the local owner's request. Interrupted/failed model turns remain explicit; they are not silently rerun.

Opt-in live-model check (uses your existing logins and can incur model usage):

```sh
make worker-smoke
```

## Trust and limits

Content is encrypted on clients before mailbox storage, using per-recipient HPKE key wrapping and authenticated payload encryption. Administrators sign membership/role changes in a chain rooted in the founder fingerprint. The founder's daemon, as the authority, sees routing metadata, member identities, sizes and timing, and remains trusted for freshness/availability; relays see only encrypted QUIC. This is not an independently audited cryptographic product and does not claim forward secrecy or compromised-device recovery.

The founder's daemon must be reachable for new authorization, even on direct peer transfers. While it is offline, local reading and queueing work; admissions and delivery wait. Revocation prevents new authorization, not access to bytes a member already downloaded. Stored messages/files have a seven-day retention window; capacity errors are explicit. The first implementation supports one team and one device identity per state directory, with any number of agents and sessions under it. Keep private state backups: identity recovery and multi-device sync are not implemented.
