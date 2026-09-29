# Native command line

> **Reference.** This covers the `shadowcode` command line. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md).

The same `shadowcode` executable runs tasks in a terminal and opens the desktop.
CLI commands start before GTK or WebKit: they need no display, Python runtime,
Node runtime, browser, or HTTP listener. Tasks use the same targets as the desktop picker: a vendor CLI (`cli:*`), a GGUF model on the bundled llama.cpp runtime (`local:gguf:*`), an OpenRouter model (`api:openrouter:<slug>`, once a key is saved), or a configured compatible endpoint. The archived [release gates](archive/NATIVE_MIGRATION.md) record its
earlier verification.

For a fullscreen workspace, use [`shadowcode tui`](NATIVE_TUI.md).

## Start a task

Build the executable using the [desktop development guide](NATIVE_DESKTOP.md).
Use a separate profile while testing the native migration:

```sh
./target/debug/shadowcode --profile /tmp/shadowcode-dev --workspace "$PWD" trust
./target/debug/shadowcode --profile /tmp/shadowcode-dev models
./target/debug/shadowcode --profile /tmp/shadowcode-dev models --use gpt-oss:20b
./target/debug/shadowcode --profile /tmp/shadowcode-dev run "Explain this project"
```

Global `--workspace` (`-p`, `--project`) selects a project; CLI commands default
to the current directory. `--profile` isolates settings, history and secrets.
With no subcommand, or with `ui`, the executable opens its native window.
The AppImage accepts the same commands after `--appimage-extract-and-run`.

`run` streams model text and reports tools, routing and selected workflows.
Use `--session <ID-or-unique-prefix>` to continue a saved conversation,
`--model <ID>` to override routing, `--purpose planner|coder|reviewer|tester`
to select a route, and `--queue` to wait behind an active task in the project.
Planning and review retain their read-only permissions.

Computer and workspace questions are grounded in the tools supplied to the
model. On Linux, the read-only `system_info` tool reports the OS and connected
kernel display outputs (including a separate enabled count); it cannot inspect
screen contents. If the host interface is unavailable or incomplete, counts are
reported as unknown rather than guessed.

`command <name> "arguments"` invokes a native slash command; no name lists the
catalog. `skill --list` lists project skills and
`skill <name> "arguments"` runs one with recorded source provenance. Desktop
navigation commands describe their requested panel/action in CLI output; they
do not remotely open desktop panels. See [workflows](NATIVE_WORKFLOWS.md).

`memory [note]` reads or appends project notes; `memory --task TASK_ID [note]`
uses an exact task in the current project. `--json` exposes content hashes;
`--replace --expected-hash HASH "text"` edits that exact version, with an empty
string clearing it. See [native memory](NATIVE_MEMORY.md) for persistence,
legacy notes, continuation, branching and export.

`sqlite PATH [SQL]` lists tables or reads an existing project database.
`--params '[100]'` binds positional placeholders; `--limit 50` and
`--timeout-ms 2000` reduce result/work limits. Check `truncated` in the response.
See [SQLite inspection](NATIVE_SQLITE.md) for types, limits and WAL behavior.

`hooks` lists project lifecycle command definitions. Review the command and its
hash, then use `hooks --enable PATH --hash HASH` to enable those exact contents
or `hooks --disable PATH` to remove the registration. Hooks remain inactive in
Plan/Review. Running tasks keep their configuration snapshot; cancel the task
to interrupt an active command. See [native hooks](NATIVE_HOOKS.md) for events,
failure handling, migration and trust boundaries.

## Approvals and output

Tool approval is explicit. An interactive terminal asks whether to allow the
exact operation; only `y` or `yes` grants it. `--interactive` requires terminal
stdin and also enables prompts when structured output is selected.

In noninteractive mode a pending approval stops the task and returns exit code
2 by default. `--approval wait` leaves the task waiting for a decision through
the desktop or a second CLI:

```sh
shadowcode approvals
shadowcode approvals --session SESSION_ID --id APPROVAL_ID --decision approve
```

`--approval approve` grants every approval request of the task this command
started, printing each one on stderr. It is for machines that are discarded
after the job, such as GitHub-hosted runners (the
[GitHub Action](../integrations/github-action/README.md) uses it when asked);
actions the project's permissions deny stay denied, and it never approves
another client's tasks.

Decisions are scoped to the saved session and operation. This command does not
grant blanket permission for later commands. `exec "command"` is an exact
user-requested terminal operation; project trust and configured restrictions
still apply.

`--json` prints one structured final result. `run --events` prints ordered
newline-delimited JSON records with `type: event`, then one `type: result`
record containing `exit_code` and `result`. Human terminal output removes
control characters; JSON preserves them through escaping. A closed output pipe
returns a normal error and cancels the task started by that CLI. Watching an
existing job does not take ownership of it.

Invalid command-line syntax is reported by the argument parser on stderr before
the engine starts. These usage errors are not JSON result records.

Exit codes are 0 for success, 1 for a failed task/command, 2 for approval needed
or invalid command-line syntax, and 130 for interruption. A model's prose is
not a guarantee of correctness; inspect recorded tool and verification results.
The result of `exec` includes the subprocess's actual exit code.
`serve` returns 0 after orderly shutdown. The source-built AppImage runtime
forwards ordinary shutdown signals, waits for native cleanup, and preserves the
payload's exit status. Force-killing the wrapper still reports its signal status;
the native child observes wrapper loss and shuts down.

`worktree --review-repair ID` and `worktree --repair ID --repair-hash HASH`
restore reviewed missing Git connection files while preserving files and staging.

See [native worktrees](NATIVE_WORKTREES.md) for creation, reviewed removal,
missing-checkout recovery, reviewed copying of uncommitted edits, and reviewed
return of committed work. `worktree --review-changes` followed by
`worktree --copy-changes --copy-hash HASH` preserves the source staging split
and copies reviewed edits into a new checkout.
`worktree --review-return ID` inspects incoming changes;
`worktree --return-changes ID --return-hash HASH` prepares them in the source
index without committing. Conflicts return a nonzero exit status and preserve
the Git merge state for explicit resolution or abort.

## Saved work and settings

```sh
shadowcode sessions "search text"
shadowcode sessions SESSION_PREFIX --rename "New title"
shadowcode export --session SESSION_ID --format json --output conversation.json
shadowcode jobs
shadowcode jobs JOB_PREFIX --watch
shadowcode jobs JOB_PREFIX --cancel
shadowcode checkpoints --session SESSION_ID
shadowcode checkpoints --session SESSION_ID --undo
shadowcode goal "Improve test coverage" --run
shadowcode goals --resume GOAL_PREFIX
shadowcode goals --pause GOAL_PREFIX
shadowcode config ui.theme dark
shadowcode health --test-model
```

Conversation and job IDs (including unique prefixes) resolve against the complete
profile history, independently of recent-list limits. Ambiguous prefixes are
rejected. Selecting the default conversation searches the current project directly,
even when other projects have more than 10,000 newer conversations. Job lookup
reads only the matching record instead of transferring recent jobs and their results.

Exports traverse the full saved history within the service's 32 MB limit.
File output uses an atomic write; redirected stdout preserves exact export
bytes. Human display on a terminal strips control characters. With global
`--json`, stdout contains the export's structured wrapper instead.
Session deletion uses `sessions SESSION_PREFIX --delete`; it removes saved
conversation data and should be used deliberately.

Model registration requires `--use` and `--provider`; `--endpoint`,
`--api-key-env`, and `--context-limit` are optional registration fields.
For example:

```sh
shadowcode models --use my-model --provider local --endpoint http://127.0.0.1:1234/v1
```

`models --no-detect` reads the saved catalog without network discovery. `config`
reads all settings, `config key` reads a dotted key, and `config key value`
validates and saves JSON or text. Use the configured environment variable or
desktop secret editor for credentials; never put secret values in shell history.
`shadowcode config updates.check false` turns off the daily update check
(Settings › About). Every key, its default and its allowed values are in
[config.example.yaml](../config.example.yaml); a value that can't be used is
refused with its key and the fix.

Backups, restore, repair and reset ([Your data](DATA.md)):

```sh
shadowcode backup                        # into ~/.local/share/shadow-agent/backups
shadowcode backup --include-secrets -o /media/usb   # API keys too: keep it private
shadowcode restore /media/usb/shadowcode-backup-20260929-120000          # check only
shadowcode restore /media/usb/shadowcode-backup-20260929-120000 --yes    # restore
shadowcode doctor --repair
shadowcode reset                         # shows what would move
shadowcode reset --yes
```

`restore` and `reset` finish right away when no ShadowCode runs on the
profile; otherwise they are scheduled for the next start (cancel in Settings ›
Your data). A restore first backs up what it replaces; `--include-secrets`
also restores the backup's API keys. `reset` moves settings and history
aside into `<folder>.reset-<time>` folders and deletes nothing.

Shell completions come from the same definitions as `--help`; the Debian
package installs them, and for the AppImage:

```sh
shadowcode completions bash > ~/.local/share/bash-completion/completions/shadowcode
shadowcode completions zsh > ~/.zfunc/_shadowcode    # a folder on your fpath
shadowcode completions fish > ~/.config/fish/completions/shadowcode.fish
```

`man shadowcode` (Debian package) lists every command, the files ShadowCode
uses and the update-check switches.

For [native MCP stdio and HTTP integrations](NATIVE_MCP.md), register a JSON/YAML
definition, inspect its command or endpoint and hash, and explicitly enable it for the
selected project:

```sh
shadowcode mcp add /path/to/definition.json
shadowcode --json mcp
shadowcode mcp enable config:my-tools --hash HASH_FROM_CATALOG
shadowcode mcp disable config:my-tools
shadowcode mcp remove config:my-tools --hash HASH_FROM_CATALOG
```

Registration is inert. `mcp add --hash CURRENT_HASH` replaces an existing global
definition. Project files have IDs such as `project:.shadowcode/mcp/my-tools.yaml`.
Each agent call still uses the same exact-argument approval flow as the window;
noninteractive tasks stop for approval instead of accepting it automatically.
Disabling affects new tasks; cancel running and queued tasks to end their
existing grants. HTTP definitions use `url` and an optional `api_key_env` bearer
secret reference; remote hosts require network access in Permissions.

To expose ShadowCode itself to another MCP client, use `mcp serve` over stdio, or
`mcp serve --http 127.0.0.1:8765 --token-env SHADOW_MCP_HTTP_TOKEN` for authenticated
loopback HTTP. Set that environment/profile secret before starting the gateway.
`mcp register` prints generic JSON for that executable, project and explicit
profile. `--client claude|cursor|codex` selects a client-specific format; Codex
uses TOML. For a running gateway, add `--url URL --token-env NAME` to emit the
client's credential-variable reference without reading its value. Both default to read-only access. `--allow-write` delegates changes
within the trusted project's configured permissions; adding `--allow-approvals`
also delegates individual approvals for that MCP owner's tasks. HTTP reconnects
share the same gateway owner; separate gateways keep clients independent. Run
`mcp serve` without `--json` so stdout remains protocol-only. See the
[native server's tools, lifecycle and remaining limits](NATIVE_MCP.md#connect-another-coding-tool-to-shadowcode).

## One engine per profile

`understand [--save]`, `doctor [--test-model]`, and `why [path] [--count N]`
provide native project inspection, diagnostics and recorded change history.
They also have `/understand`, `/doctor`, and `/why` desktop commands. Inspection
is read-only by default; saving preserves existing project notes. Diagnostics
contact a model only when `--test-model` is explicit. See the
[workflow details and limits](NATIVE_INSPECTION.md).

MCP delegates jobs through a private ownership connection to this engine.
Gateway termination, including SIGKILL, cancels its owned running and queued work.
The engine records ownership before scheduling, including when the submission
reply is lost. Up to eight such connections can be active, leaving capacity for
ordinary control requests. Explicitly detached CLI tasks are independent and
continue until completion or cancellation.

The AppImage runtime extracts each invocation into a private temporary directory.
Simultaneous desktop and CLI launches can use the same `TMPDIR`; finishing one
launch does not remove another's application files. See the
[runtime build and checks](../packaging/native-runtime/README.md).

When the desktop is open, CLI requests share its task engine, queue, approvals,
history, and background manager. Each request has its own project/session
selection, so a command in another project does not change the visible desktop
or its remembered project. Global model/settings changes intentionally apply to
that profile.

Without a persistent owner, the CLI opens a temporary engine and closes it when
the command ends. Other clients may inspect that foreground task, approve its
tools, or cancel it. Starting separate concurrent work is rejected because this
temporary owner will exit. To keep tasks and servers running, open the desktop
first, or run the explicit foreground headless owner in another terminal:

```sh
shadowcode serve
# In another terminal, with the same profile:
shadowcode run "Inspect this project" --detach
shadowcode background start --name dev --command "npm run dev"
shadowcode background list
shadowcode background logs PROCESS_PREFIX
shadowcode background stop PROCESS_PREFIX
```

`--detach` and background start require that persistent owner. Closing it cancels
managed work and waits for process-group cleanup. `serve` and the desktop also
run [scheduled automations](AUTOMATIONS.md); one-shot commands such as `run`
never do. `serve` does not install a
daemon or automatically restart jobs. Opening the GUI while `serve` or a TUI
owns the same profile attaches to that engine. The window keeps its own project
and conversation selection and shows a notice that closing it leaves shared
work running. A temporary foreground command cannot host an attached desktop.

`shadowcode serve --remote` also serves the web interface for phones and other
computers (on the saved address, `127.0.0.1:7390` by default;
`--remote-address IP:PORT` for this run only) and prints a one-time pairing
link with its QR code. `shadowcode remote` shows status and paired devices,
`shadowcode remote pair [--host IP]` prints a new pairing link, and
`shadowcode remote revoke ID_PREFIX` or `--all` unpairs devices. See
[REMOTE.md](REMOTE.md).
See [desktop attachment](NATIVE_DESKTOP.md#attaching-to-a-running-engine).

Model [background tools](NATIVE_BACKGROUND.md#asking-a-model-to-manage-a-server)
use this same process list. On a persistent owner, a started project server
survives coding-task completion or cancellation; stop it through the panel,
`background stop`, or an approved model stop request. A temporary CLI owner
stops its project processes when the command exits.

Ctrl-C, SIGTERM, or SIGHUP cancels a task started by that CLI, pauses a running goal, and
cancels an active manual command. During `jobs --watch` it only stops watching;
the existing job continues. A disconnected manual-command client drops that
operation and cleans its process group. As with the desktop, a user-level
process runner cannot contain a subprocess that deliberately detaches into a
different session. An inherited ignored SIGHUP remains ignored, so `nohup`
continues to work.
Extraction mode also follows the AppImage wrapper's lifetime, so signalling only
the wrapper PID does not leave native tasks or servers running as orphans.

### Editors over ACP

`shadowcode acp [--trust] [--print-config zed|jetbrains|generic]` serves the
[Agent Client Protocol](ACP_SERVER.md) on stdin/stdout so Zed, JetBrains IDEs
and other ACP editors can run ShadowCode threads. It attaches to the desktop
or `serve` engine when one is open; otherwise it owns the engine (mode `acp`
in `/api/runtime`) and serves the same socket, so a desktop opened meanwhile
attaches to it like it does to a TUI. Each editor prompt is a job owned by
that connection, so quitting the editor cancels it. `--trust` trusts a folder
the first time an editor opens a session there; without it, untrusted folders
are refused with instructions. `--print-config` prints the editor entry for
this executable (with `--profile` and the AppImage flag when used) and exits.
`--json` is refused because stdout carries JSON-RPC.

The connection is a private Unix socket keyed to canonical config/data/state
paths, inside `/run/user/<uid>/shadowcode` or `/tmp/shadowcode-<uid>`. Directories
must be owned by the user and mode 0700; sockets use 0600. Both peers verify the
OS user, and requests identify their protocol and profile. Frames, concurrent
clients and idle reads are bounded. The connection exposes no TCP port and
accepts no browser HTTP requests. Different application versions refuse to share
an engine. Existing non-socket files and active endpoints are never overwritten.

The native full-screen TUI, updater/automatic diagnostic repairs, and remaining integration migration
remain in progress; their 0.19 commands are not silently emulated here.

## Verification

`node scripts/test-native-cli.mjs` runs the actual executable without display
variables. It uses a disposable profile and scripted local model, checks actual
PTY approval/denial/interruption, task and event output, continuation, exports,
checkpoints, skills, goals, sharing a headless owner, background processes,
broken-pipe cancellation, and process cleanup. It needs util-linux `script` for
the PTY checks. Results go to `artifacts/native-cli/`.

Set `SHADOW_DESKTOP_BINARY` to an AppImage,
`SHADOW_CLI_ARGS='["--appimage-extract-and-run"]'`, and
`SHADOW_CLI_ARTIFACTS` to a separate directory to test that exact package.
The real-window test also invokes the CLI in another project while the desktop
owns the engine and verifies isolation and shared background controls.


## Project plugins

`plugin` lists available and installed native bundles for the selected project.
`plugin inspect NAME` previews all files; `plugin install NAME --hash HASH`
requires the reviewed bundle hash. Both accept `--file bundle.json` for custom
bundles. `plugin remove NAME --hash HASH` uses the current installation hash
from `plugin --json` and reports modified files it preserved. Installation never
runs commands or activates hooks/MCP. See [native plugins](NATIVE_PLUGINS.md)
for schema, examples, recovery and legacy migration.
