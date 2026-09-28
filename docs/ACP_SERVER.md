# ShadowCode in your editor (ACP)

> **Reference.** `shadowcode acp` makes ShadowCode an [Agent Client
> Protocol](https://agentclientprotocol.com) agent. Zed, JetBrains IDEs and
> any other ACP client can then run ShadowCode tasks in their agent panel,
> with ShadowCode's own models, tools, sandbox, approvals and history.

ACP is JSON-RPC 2.0 over stdio, one message per line. The editor starts
`shadowcode acp` as a child process and talks to it on stdin/stdout.
ShadowCode speaks protocol version 1. Nothing but JSON-RPC is written to
stdout; diagnostics go to stderr, which editors show in their agent logs.

## Set up

Print a ready entry for your editor. It names the exact executable (and the
AppImage flag when you run the AppImage), plus `--profile` if you pass one:

```sh
shadowcode acp --print-config zed        # Zed settings.json → agent_servers
shadowcode acp --print-config jetbrains  # JetBrains ~/.jetbrains/acp.json
shadowcode acp --print-config generic    # command + args for anything else
```

Add `--trust` to the print command to include it in the entry (see
[Trust](#trust)).

### Zed

Open **Zed › Settings › Open Settings** (`settings.json`) and add:

```json
{
  "agent_servers": {
    "ShadowCode": {
      "type": "custom",
      "command": "/usr/bin/shadowcode",
      "args": ["acp"],
      "env": {}
    }
  }
}
```

Use the path `--print-config zed` printed. For the AppImage it is the
`.AppImage` file with `"args": ["--appimage-extract-and-run", "acp"]`. Older
Zed builds that reject `"type": "custom"` accept the same entry without that
line. Then open the agent panel, choose **New ShadowCode Thread**, and write a
request. The thread works in the project folder Zed has open.

### JetBrains IDEs

AI Assistant reads custom ACP agents from `~/.jetbrains/acp.json` (its agent
menu has an entry to add a custom agent, which opens that file). Add:

```json
{
  "agent_servers": {
    "ShadowCode": {
      "command": "/usr/bin/shadowcode",
      "args": ["acp"],
      "env": {}
    }
  }
}
```

ShadowCode then appears in the AI Assistant agent menu.

### Other ACP clients

Anything that launches an ACP agent over stdio works: the command is
`shadowcode` with the argument `acp` (put `--profile DIR` before `acp` for an
isolated profile). `--print-config generic` prints exactly that.

## Trust

ShadowCode only runs tasks in folders you have trusted. When the editor opens
a session in an untrusted folder, `session/new` fails with JSON-RPC error
`-32602` (`data.reason = "untrusted_workspace"`) and a message telling you how
to trust it:

- open the folder in the ShadowCode window and choose **Trust**, or
- run `shadowcode --workspace /path/to/project trust`, or
- start the agent with `shadowcode acp --trust`, which trusts a folder the
  first time an editor opens a session in it. Use this only with editors you
  launch yourself on projects you would open in ShadowCode anyway.

## What you get

| Editor feature | ShadowCode behaviour |
| --- | --- |
| Threads (`session/new`) | A new ShadowCode conversation in the editor's project. It also appears in the desktop's history. |
| Resume (`session/load`) | Replays the conversation: your messages, answers, tool calls with their results and the plan. `session/resume` reopens without replay. |
| History list (`session/list`) | Conversations of a project, newest first, with title and last update. |
| Prompt text | The task. Several text blocks are joined with newlines. |
| `@file` mentions (resource links) | When the editor offers `fs/read_text_file`, ShadowCode asks it for the file, so unsaved changes are included. Otherwise a project file or folder becomes a ShadowCode @-mention (its saved text, or the folder listing, is read when the task starts; at most 20); other links are listed as references. |
| Embedded context (resources) | Included as attached context, marked as data rather than instructions. |
| Images (PNG, JPEG, WebP, GIF) | Saved under `.shadow/attachments/` and sent with the turn (the chosen model must accept images). Audio is refused. |
| Streaming | Answer text arrives as `agent_message_chunk`; status notes (model used, retries, compaction, warnings) as `agent_thought_chunk`. |
| Tool calls | `tool_call` / `tool_call_update` with a kind (`read`, `edit`, `delete`, `move`, `search`, `execute`, `fetch`, `think`, `other`), file locations, diffs for edits, and the output of commands and searches. |
| Plan | `update_plan` becomes the editor's `plan` view. |
| Permissions | Every ShadowCode approval becomes `session/request_permission` with **Allow**, **Allow … for this task** (ShadowCode's own grant: the same kind of action, or the same command prefix such as `cargo test`, for the rest of this prompt; offered only when ShadowCode can grant it) and **Reject**. Your answer is applied through the engine's approvals, with the diff or command preview shown. A handoff to another provider asks the same way. |
| Busy project | A prompt sent while another task runs in the same project (another thread, or the desktop) waits in ShadowCode's queue and starts when that task ends; a thought note says so. Cancel works while it waits. |
| Cancel (`session/cancel`) | Stops the running task; the prompt ends with `stopReason: "cancelled"`. |
| Modes | **Code** (edit with approvals), **Plan** (read-only planning) and **Ask** (read-only answers). Offered both as `modes`/`session/set_mode` and as the `mode` config option. |
| Models | The desktop picker's ready rows — subscriptions, models on this computer, configured endpoints and OpenRouter (featured first, 150 rows at most) — as the `model` config option (`session/set_config_option`) and the unstable `models` / `session/set_model`. A choice is remembered for the conversation, exactly like picking it in the desktop. |
| Titles | After a turn, a new conversation title is sent as `session_info_update`. |

A turn ends with `stopReason`:

- `end_turn` — the task finished (also when a subscription reached its plan
  limit; the message explains it);
- `cancelled` — you cancelled it;
- `max_turn_requests` — the task reached its step limit;
- otherwise the prompt fails with JSON-RPC error `-32603` carrying the
  task's error, `data.jobId` and `data.status`.

File reads, edits and shell commands always use ShadowCode's own tools, its
sandbox and its checkpoints (so **Rewind** in the desktop works for turns run
from an editor). ShadowCode never needs the editor's `fs` or `terminal`
capabilities.

### Not supported

- **Editor-provided MCP servers** (`mcpServers` in `session/new`) are not
  launched; a note goes to stderr. Register servers once with
  `shadowcode mcp add` and enable them per project (see
  [MCP](NATIVE_MCP.md)); ShadowCode's own review of each server definition
  applies to editor threads too.
- Audio prompts, `session/delete` and `logout` are not offered. Authentication
  is not required (`authMethods` is empty); subscriptions and API keys are
  set up in ShadowCode itself.
- Slash commands are not advertised to the editor yet; type the request
  instead.

## Running next to the desktop

A profile has one engine. `shadowcode acp` uses whichever is running:

- **Desktop or `shadowcode serve` open:** the agent attaches to it over the
  private local socket. Editor threads share its queue, approvals, history and
  settings; the desktop shows them live and can approve their tools too.
- **Nothing open:** the agent opens the engine itself and serves the same
  socket, so a desktop started later attaches to it (and says so). When the
  editor closes the agent, that engine closes; an attached desktop then needs
  to be reopened.

Each prompt runs as a job owned by the editor's connection. If the editor
quits or the agent process is killed, its running prompts are cancelled
(other work in the engine is not touched). At most eight owner connections
(editor prompts and MCP gateways together) can run at once. A running prompt
reads its task's progress when the engine announces a change (through the
same wake-up feed an attached desktop window uses, which counts as one of
its four views), with a read every two seconds as a backstop; if no view is
free it reads every 100 ms instead. Settings and
history are written through the single engine, so the desktop and any number
of editors never write the profile's database from two processes.

A foreground `shadowcode run` without `--detach` owns its profile only for
that command; while it runs, editor prompts are refused until it finishes.

## Protocol details

- `initialize` answers `protocolVersion: 1` (the latest this agent speaks
  when the client asks for another), `agentCapabilities.loadSession: true`,
  `promptCapabilities {image: true, audio: false, embeddedContext: true}`,
  `mcpCapabilities {http: false, sse: false}`,
  `sessionCapabilities {list, resume, close}`, `authMethods: []` and
  `agentInfo {name: "shadowcode", title: "ShadowCode", version}`.
  Other methods before `initialize` fail with `-32600`.
- The session id is the ShadowCode conversation id.
- Errors: `-32700` malformed JSON, `-32600` invalid message (or a line over
  48 MB), `-32601` unknown method, `-32602` invalid parameters (untrusted or
  relative `cwd`, unknown mode or model, empty prompt, a second prompt while
  one is running), `-32002` unknown session, `-32603` a failed task or engine
  error.
- Approvals the editor has not answered when the engine resolves them
  elsewhere (the desktop answered, the approval expired, the task stopped)
  are withdrawn with `$/cancel_request`.
