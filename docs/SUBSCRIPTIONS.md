# Subscriptions

ShadowCode runs coding tasks on the subscriptions you already have by starting
each vendor's official command-line tool in the project folder. The vendor's
agent runs the loop with its own tools and sandbox. ShadowCode provides the
workspace, the transcript, approvals it receives, review, and steering. It
never injects its own tools or bubblewrap into a vendor process, and it never
reads, stores or proxies vendor credentials.

## Setup

Each CLI must be on `PATH`, or its path set in **Settings › Advanced › Vendor
tools** (`cli_agents.<vendor>_binary`). Antigravity is the exception: its agent
server is installed from **Settings › Accounts** (see
[below](#antigravitys-agent-server)).

| Vendor | Binary | Install | Sign in |
| --- | --- | --- | --- |
| Codex | `codex` | `npm i -g @openai/codex` | **Connect** runs `codex login` |
| Claude Code | `claude` | [Claude Code](https://code.claude.com/docs/en/headless) | **Connect** runs `claude auth login` |
| Cursor | `cursor-agent` | [Cursor CLI](https://cursor.com/docs/cli/acp) | **Connect** runs `cursor-agent login` |
| Antigravity | `agy_acp_server.par` (Google's ACP agent server) | **Install** in **Settings › Accounts** downloads Google's official agent server (334 MB from dl.google.com, checked against a pinned SHA-256) | **Connect** signs in with Google in the browser |
| Grok | `grok` | [xAI CLI](https://docs.x.ai/build) | **Connect** runs `grok login` |

- **Connect** runs the login command with your environment, minus provider API
  keys. The vendor opens your browser or prints a URL or device code, and
  **Settings › Accounts** shows those lines. One sign-in per vendor can run at
  a time. It can be cancelled and stops after 10 minutes. When it finishes,
  ShadowCode checks the account again.
- **Returning to Accounts.** Active sign-ins are recovered without starting
  another login. Each provider retains its own instructions and Cancel
  control. If cancellation is already in progress, the card continues to
  show **Stopping…** until the provider operation finishes. An unavailable
  progress read keeps cached account information; use **Refresh** to request
  another explicit check.
- **Disconnect** asks for confirmation, then runs `codex logout`,
  `claude auth logout`, `cursor-agent logout` or `grok logout`. Those sign the
  CLI out everywhere on this computer, not only in ShadowCode. ShadowCode then
  forgets the vendor's cached status, stored usage snapshots and every
  conversation's stored vendor session. For Antigravity, Disconnect deletes
  ShadowCode's private Antigravity profile (its Google sign-in); the `agy` CLI
  and the Antigravity app keep their own sign-ins.
- **Refresh** checks one vendor again. Otherwise each vendor is checked at
  most once every 5 minutes, and failures back off. In *Offline* mode no vendor
  process is started for status, models or usage.

A row is **Ready** only after the vendor's own status check says so. An
installed binary or a login file is not enough.

## Antigravity's agent server

Antigravity runs through Google's official ACP agent server, the one listed in
the [ACP registry](https://github.com/agentclientprotocol/registry) and used by
other editors. It asks ShadowCode before it runs commands or edits files, which
the `agy` CLI's print mode could not do.

- **Install.** **Settings › Accounts › Antigravity › Install** downloads
  version 1.2.1 from `dl.google.com` (334 MB), checks its size and SHA-256,
  and unpacks it (about 1.1 GB) into
  `~/.local/share/shadowcode/antigravity-acp/1.2.1`. **Remove agent** deletes
  it. To use a copy you manage yourself, set `cli_agents.antigravity_binary`
  to its `agy_acp_server.par` path (with `localharness_external` beside it).
- **Sign-in.** The server keeps its Google sign-in in a private profile,
  `~/.local/share/shadowcode/antigravity-acp/profile`, separate from the `agy`
  CLI and the Antigravity app. **Connect** lets it open your browser;
  ShadowCode also shows the link. Status checks and tasks never open a browser:
  if the sign-in is missing or expired, the row says *Sign in* and a task
  stops with that message.
- **Models.** The picker lists the session's `model` config option. A picked
  model is set with `session/set_config_option`, and the prompt is sent only
  after the server confirms the switch (Cursor's `session/set_model` works the
  same way). If the server rejects the model, the task fails and asks you to
  pick it again.
- **Temp files.** Each launch gets its own temp directory under
  `~/.local/share/shadowcode/antigravity-acp/runs`, removed afterwards.
  Directories a crash left there are removed at the next launch once they
  are a day old.
- **Hosts without IPv6.** The server refuses to start without an IPv6
  loopback (`::1`); on such hosts ShadowCode passes the server's own
  `--enforce_kernel_ipv6_support=false` switch.

## What each vendor exposes

| | Codex | Claude Code | Cursor | Antigravity | Grok |
| --- | --- | --- | --- | --- | --- |
| Runtime | `codex app-server` (JSON-RPC 2.0) | `claude -p --output-format stream-json --input-format stream-json --verbose --include-partial-messages --permission-prompts host` | `cursor-agent acp` (Agent Client Protocol) | `agy_acp_server.par --uid=` (Agent Client Protocol) | `grok agent stdio` (ACP) |
| Sign-in check | app-server `account/read` (falls back to `codex login status`) | `claude auth status` | ACP `initialize` → `authenticate` → `session/new` | ACP `initialize` → `authenticate` (`oauth-personal`) → `session/new`; a printed Google sign-in link means *Sign in* | ACP handshake (falls back to `grok models`) |
| Models | app-server `model/list` | *Default* plus the aliases (`fable`, `opus`, `sonnet`, `haiku`) that the installed `claude --help` lists | ACP session models. Exact IDs, including bracketed parameters, set with `session/set_model`. *Auto* is Cursor's own router | The `model` option of `session/new`'s `configOptions`, set with `session/set_config_option` | ACP session models |
| Image input | Yes (`localImage`), for models that accept images | Yes (base64 image blocks) | When ACP `initialize` reports image support | Yes: ACP `initialize` reports image support | No: ACP reports `image: false` |
| Approvals reach ShadowCode | Yes: command and file-change requests | Yes: `can_use_tool` control requests | Yes: `session/request_permission` | Yes: `session/request_permission` (questions it asks are skipped with a note) | Yes: `session/request_permission` |
| Plan/Review (read-only) | `sandbox: read-only` | `--permission-mode plan` | ACP mode `plan`, when offered | ShadowCode denies its requests | Not enforced by Grok |
| Resume | `thread/resume` | `--resume <session>` | `session/load` | `session/load` | `session/load` |
| Usage shown | Plan rate-limit windows per quota pool | *Usage unavailable* | *Usage unavailable* | *Usage unavailable* | *Usage unavailable* |

Grok isn't one of the featured vendors: its rows are listed after the others.

## Usage

ShadowCode shows only what a vendor reports. It never estimates a figure.

- **Codex** reports rate limits through app-server `account/rateLimits/read`,
  and pushes `account/rateLimits/updated` during a turn. A row shows the
  windows of its quota pool (for example a 5-hour and a weekly window), percent
  used and remaining, reset times, the plan, and credits when Codex reports
  them. Models that share a pool show the same numbers. Some models draw on a
  separate pool.
- **Claude Code** has no plan usage interface for other apps. The row says
  *Usage unavailable* and links to `claude.ai/settings/usage`.
- **Cursor** reports its plan tier but no remaining allowance, so the row says
  *Usage unavailable*.
- **Antigravity**'s agent server reports no plan usage to other programs.
- **Grok** reports per-session token counts only.

After a restart, the last Codex snapshot is loaded from the database and marked
*Last checked …* until the next check. A snapshot older than 30 minutes is
marked stale. If a CLI is signed in with an API key (Codex `auth_mode: apiKey`,
or a Claude auth method other than claude.ai), its rows say
*API key login · billed per token* and show no plan usage.

**API keys are removed.** `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
`OPENAI_API_KEY`, `CODEX_API_KEY`, `CURSOR_API_KEY`, `XAI_API_KEY`,
`GROK_API_KEY`, `GEMINI_API_KEY` and `GOOGLE_API_KEY` are removed from the
environment of every vendor task, login and logout. This way a subscription
row is never billed per token behind your back.

## Plan limits

If a vendor reports a plan limit, the job stops with the status
`limit_reached`. ShadowCode doesn't retry. The detected messages include Codex
`usageLimitExceeded`, "usage limit", "rate limit reached" and "quota exceeded".
The affected rows become unavailable with the reason, and a banner offers
**Choose model**. ShadowCode never buys credits, redeems resets or turns on
overages.

## Codex exec fallback

If `codex app-server` is missing, or fails before the first turn is sent,
ShadowCode can run the turn with `codex exec --json`. That path has no approval
channel. ShadowCode refuses it whenever shell commands are set to ask
(`permissions.approve_shell`, on by default in both permission modes), and
never uses it once a turn has started or when images are attached.

## What ShadowCode does

- **Transcript.** Vendor text, tool starts and finishes, file-change reports,
  results and errors go into the transcript. Output is redacted and clipped
  before it is shown or stored. A streamed reply is shown once; the final
  result doesn't repeat it.
- **Approvals.** Vendor approval requests go through the same Allow/Deny cards
  as ShadowCode's own. In read-only tasks they are denied automatically with a
  warning. After `cli_agents.approval_timeout_sec` (600 s) without an answer,
  a request is denied.
- **Stop.** Stop ends the vendor's whole process group: SIGTERM, then SIGKILL.
  A vendor that prints nothing for `cli_agents.stall_timeout_sec` (900 s) is
  stopped and the task fails.
- **Pause and steer.** Pausing interrupts the vendor turn where the protocol
  allows it. The steering note is sent as the next prompt.
- **Rewind.** Before each turn (not in Plan/Review), ShadowCode checkpoints
  the project. In a Git repository, that is a commit built in a temporary
  index under `refs/shadowcode/checkpoints/<session>/<n>`. In other folders,
  a bounded copy is taken. After the turn, every project file the CLI changed
  is recorded in the task's checkpoint, so **Rewind** restores it once the
  turn has ended. The same limits as for shell commands apply: Git-ignored
  files, files over 4 MB, symlinks and Git history aren't restored. A folder
  without Git that is too large for the copy gets a warning instead. Edits
  you make yourself in those files during the turn are rewound too. The
  vendor's own conversation isn't told about a rewind. Set
  `checkpoints.vendor: false` to turn this off.
- **Switching providers.** A move between providers hands over at most 12,000
  characters of earlier turns, after you consent. See the
  [user guide](USER_GUIDE.md#switch-models-mid-conversation).
- **MCP servers.** The MCP servers you enabled for the project are passed to
  the vendor for each run, in addition to the vendor's own MCP settings:
  Cursor, Grok and Antigravity receive them in ACP `session/new` and
  `session/load`, Claude Code through `--mcp-config`, Codex through
  `-c mcp_servers.<name>.command/args/url`. Nothing is written to the vendor's
  configuration files. Servers that need stored secrets or literal
  environment values are not shared, and Plan/Review tasks share none.
  `mcp.share_with_cli_agents: false` turns this off. See
  [Subagents](SUBAGENTS.md#mcp-servers).

## Settings

These are the `cli_agents` keys in `config.yaml`. **Settings › Advanced ›
Vendor tools** edits all of them except `claude_enabled`, which you set with
`shadowcode config cli_agents.claude_enabled false` or in the file.

- `enabled`: master switch for all vendor CLIs.
- `claude_enabled`: turns the Claude adapter off. Anthropic doesn't allow
  third-party clients to use Pro/Max OAuth tokens directly. Driving the official
  `claude` binary under your own login is tolerated but not guaranteed.
- `codex_binary`, `claude_binary`, `cursor_binary`, `grok_binary`: a name on
  `PATH`, or an absolute path.
- `antigravity_binary`: the default, `agy`, means the agent server installed
  from **Settings › Accounts**. Any other value must be the path to an
  `agy_acp_server.par` with `localharness_external` beside it.
- `approval_timeout_sec` (600) and `stall_timeout_sec` (900).
