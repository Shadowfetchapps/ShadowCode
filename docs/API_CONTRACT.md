# ShadowCode application API contract (1.0)

Every ShadowCode client talks to the engine through one set of JSON routes,
`METHOD /api/...`, answered by `Service::dispatch` (`native/core/src/service.rs`):

- **The desktop window** over Tauri IPC:
  `invoke("api", {request: {method, path, body}})` (`ui/src/lib/transport.ts`).
- **The command line, the terminal UI, `shadowcode acp` and the MCP gateway**
  over the private local control socket (`native/core/src/control.rs`). Each
  request carries the client's project (and conversation), so a CLI request
  never changes the project the desktop shows.
- **Remote browsers** over HTTP when remote access is on
  (`native/core/src/remote/http.rs`), filtered by `native/core/src/remote/policy.rs`.

Requests and answers are JSON. This file supersedes `docs/API_CONTRACT_0.28.md`.
Shapes are written as JSON sketches: `string|null` means the field is always
present and may be `null`; `field?` means the field may be absent.

## Versioning policy

- **Additive within 1.x.** A 1.x release may add routes, optional request
  fields, response fields and event types. It never removes or changes the
  meaning of what is here.
- **Clients ignore what they do not know.** Clients must ignore unknown
  response fields and unknown event `type`s (and unknown wake-up types).
- **`stable` routes.** A stable route's path, method, required inputs and
  existing response fields keep their names, types and meaning for all of 1.x.
  Removals and incompatible changes wait for 2.0.
- **Deprecation.** A route or field that is being replaced is marked
  `deprecated (since 1.x)` in this file and in the CHANGELOG, names its
  replacement, and keeps working for the rest of 1.x.
- **`experimental` routes** may change or disappear in a minor release; the
  CHANGELOG says so when they do. Do not build on them without pinning a version.
- **Checked index.** The [route index](#route-index) is checked by
  `native/core/tests/api_contract.rs` against the router source: adding or
  removing a route without updating this file fails the build.

## Conventions

### Transports

| Transport | Used by | Request | Answer | Limits |
| --- | --- | --- | --- | --- |
| Tauri IPC `invoke("api")` | desktop window | `{method, path, body}` | the value, or a rejected promise with the error string | body 8 MB (below) |
| Control socket `/run/user/<uid>/shadowcode/<32 hex>.sock` (or under `/tmp/shadowcode-<uid>/`), one per profile, mode 600 | CLI, TUI, `shadowcode acp`, MCP gateway, attached desktop windows | length-prefixed JSON envelope `{protocol: 1, profile, workspace, session_id, view?, request}` | `{protocol, profile, result}` or `{protocol, profile, error}` | request frame 8.1 MB, response 68 MB; peer must be the same OS user |
| Remote HTTP | paired browsers | `METHOD /api/...` with `Authorization: Bearer <token>`, `Content-Type: application/json` | 200 with the value, or an error status with `{error}` | body 8 MiB, response 68 MB, 600 s per request |

The desktop shell adds `desktop_attached: boolean` and `desktop_pid: number`
to the answers of `GET /api/health` and `GET /api/version` it forwards
(`src-tauri/src/backend.rs`). An *attached* window is a second window that
uses an engine another process already owns (see
[Local control socket](#local-control-socket)).

### Requests

- `path` must start with `/api/` and be at most 16 000 bytes, query string
  included ("Invalid application command path"). The serialized body
  must be at most 8 000 000 bytes ("Request exceeds 8 MB").
- Query parameters are read from `path`; they are always optional unless a
  route says otherwise. `?limit=` is clamped to each route's `1..=max`.
- A `GET` sends `body: null`. A body that is not a JSON object reads as empty.
- **Lenient fields.** Most bodies are read through `Text` / `Flag` /
  `Loose<T>` fields (`native/core/src/service/call.rs`): a missing field,
  `null`, or a value of another type reads as *absent* (`""` for text, `false`
  for flags), never as an error. So `{"queue": "yes"}` means `queue: false`.
  **Strict exceptions** are marked "(strict)" in their sections. A mistyped
  field is an error in all of them, and an unknown field too in
  `POST /api/code-intel/config`, `POST /api/voice/config`,
  `POST /api/jobs/verification-refresh` and `POST /api/sqlite`. The
  automation draft, the job fields `mentions`, `context` and
  `permission_limit`, and goal `milestones` are typed but ignore unknown fields.
- **Selection.** Each client view has a *selected project* (and conversation).
  Routes that do not take a `workspace` act on it. A `workspace` body field or
  query parameter, where accepted, may start with `~` or `~/`. Desktop IPC
  shares one selection; each CLI request forks the selection it names; each
  remote device and browser tab (`X-Shadow-View`) has its own.
- **Workspace file paths** are relative to the project root (absolute paths
  inside the project are accepted); `..` escapes, symlink escapes and paths
  outside the project are refused.

### Values

- **Ids** are 32 lowercase hex characters (conversations, tasks, jobs,
  approvals, goals, automations, runs, terminals, worktrees, snapshots).
  Event ids and pin ids are integers (database row ids, increasing).
- **Times** are Unix seconds as floating-point numbers. Exceptions, which are
  strings: diagnostic export `captured_at` (RFC 3339), and dates copied from
  other services (`latest.published_at` from GitHub, issue `updated_at` and
  comment `created_at` from the forge).
- **Unknown values are `null`**, never invented: a cost nobody reported, a
  duration that was not observed, a version that could not be read.
- **Picker ids** (`model` fields) are exact routing ids from
  `GET /api/picker`: `cli:<vendor>[:<model>]`, `local:gguf:<hash>`,
  `api:openrouter:<slug>`, or a model-registry id. Display names are refused.
- **Secrets are never returned.** API keys, access tokens and their digests
  never appear in answers. Tool payloads stored in the transcript
  (`tool.started`, `tool.completed`, `approval.requested`,
  `command.completed`, `terminal.completed`) are redacted before storage.

## Errors

- **Over IPC and the control socket** an error is one string: the error and
  its causes joined with `: ` (`format!("{e:#}")`). The window's transport
  (`ApiError`) also parses a JSON object that appears in the text.
- **Unknown routes** fail with `Application command is not available: METHOD /api/path`.
- **Refusals that are answers, not errors.** Some routes answer a normal value
  that says why nothing happened, so every transport sees the same shape:
  - `POST /api/jobs` / `POST /api/run` needing handoff consent:
    `{ok: false, status: 409, error, needs_consent: true, handoff: {from, to, excerpt_chars, images, reason}}`.
    The `409` is a field of the value on every transport; remote HTTP answers
    it with HTTP 200. Nothing was written; resend with `handoff_consent: true`.
  - `POST /api/accounts/{vendor}/disconnect` without `confirm: true`:
    `{ok: false, needs_confirm: true, ran: [], note}`.
  - `POST /api/projects` for an untrusted folder: `{needs_trust: true, ...}`.
  - `POST /api/automations/preview`: `{ok: false, error}`.
  - `GET /api/issues` when issues cannot be listed: `{ready: false, ...}`.
- **Remote HTTP status codes** (`remote/http.rs`); error bodies are
  `{error: string}` (application errors are redacted like answers):

| Status | When |
| --- | --- |
| 200 | the route's value (including in-band refusals above) |
| 400 | application error from the route; invalid JSON body; absolute-form request target |
| 401 | missing, unknown or revoked token (`WWW-Authenticate: Bearer realm="ShadowCode remote access"`); unknown, used or expired pairing code |
| 403 | cross-origin request or `OPTIONS` preflight; refused by the remote policy (see each route's Remote column) |
| 404 | unknown `/_remote/...` path or missing web asset |
| 405 | method other than `GET`, `POST`, `PUT`, `PATCH`, `DELETE` on `/api`; non-GET on a page |
| 408 | request body took longer than 30 s |
| 413 | body larger than 8 MiB (4096 bytes for `/_remote/pair`) |
| 415 | body not `application/json` |
| 421 | `Host` is not an IP address, `localhost`, `*.localhost`, `*.ts.net` or the saved public address |
| 429 | 8 failed attempts from one address within 5 minutes; `Retry-After` seconds |
| 500 | answer larger than 68 MB |
| 503 | remote access is stopping; more than 32 open event streams |
| 504 | the route took longer than 600 s |

HTTP 409 is not used by the transport; handoff consent is the in-band value above.

## Events

### Wake-ups

The engine broadcasts every stored event and a few transient ones. Clients
treat broadcasts as **wake-ups** and read committed rows by cursor
(`GET /api/sessions/{id}/events?after=`, `GET /api/jobs/{id}/events?after=`,
`GET /api/feed`), so a missed wake-up loses nothing.

- **Desktop** (`src-tauri/src/main.rs`) emits:
  - `shadowcode:events` `{session_id, type}` for every broadcast except
    terminal ones, and an untyped `{}` after a lagged stream or a reattach
    (read everything again).
  - `shadowcode:terminal` `{type, terminal_id}` for `terminal.output` and
    `terminal.exited` only (never output bytes).
  - `shadowcode:shutdown` `{status: "closing"|"error", message}` while the
    window closes; `shadowcode:open-session` `{session_id}` when a desktop
    notification is clicked.
- **Remote** `GET /_remote/stream` sends the same two wake-ups as Server-Sent
  Events (see [Remote access](#remote-access)); `view.*` types are dropped.
- **Attached views** receive `{type, session_id, payload?, terminal_id?}`
  where `payload` is a bounded notification hint (`notify::hint`: summary at
  most 180 characters, success, cancelled, limit, command, tool), never
  transcript content.

The window's feed re-reads `GET /api/feed` for the types listed in its
`events` field, for `view.*` hints and for untyped wake-ups, at most once per
30 ms burst.

### Stored events

Rows in the `events` table, returned by `GET /api/sessions/{id}/events`,
`GET /api/events`, `GET /api/jobs/{id}/events` and inside
`GET /api/sessions/{id}`:

```
EventRow = {id: number, ts: number, type: string, session_id: string|null, task_id: string|null, payload: object}
```

History pages (`view=window`) replace an event whose payload exceeds 256 KB
with `type: "history.omitted"`, `payload: {text, original_type, original_bytes}`;
the original stays in the store and in exports.

Types, grouped, with the payload fields clients rely on:

- **Turn lifecycle**
  - `user.message` `{text, mentions?, only_change?}` — the task text as
    stored (mentions resolved by the engine are not inlined; Preview context
    is included); a prompt with @-mentions keeps them and its "Only change
    these" choice, which **Try on…** sends again.
  - `agent.started` `{job_id, task, mode, model, native, vendor_agent?, images}`.
  - `agent.completed` — the job's `result`: `{success, cancelled, summary,
    plan: {goal, steps}, usage, usage_is_estimated, verification, timings,
    limit_reached?, command?}`.
  - `agent.paused` `{job_id, status: "paused"}` (when the worker parks),
    `agent.steered` `{job_id, note}`.
  - `agent.warning` `{text, kind?: "sandbox"|"checkpoint"|"scope"|"compact",
    vendor?}`.
  - `agent.handoff` `{from, to, excerpt_chars, turns, files, delivery: "prompt_prefix"|"message_tape", job_id}`.
  - `workflow.selected` — the workflow or skill a slash command started.
  - `command.completed` `{name, result}` — a slash-command card (redacted);
    `terminal.completed` `{command, result}` — the Run button (redacted).
- **Model output and routing**
  - `model.stream` `{text, message_id}` — incremental text;
    `model.delta` `{text, message_id, complete}` — reply text (with
    `complete: true` the whole reply of that request); `model.stream_end` `{message_id, complete}`
    (vendor turns send it too, so the final result never repeats a reply the
    transcript already shows).
  - `model.retry`, `model.request_timing`, `model.request_metadata`,
    `model.response_metadata` — see [usage, retries and compaction](#usage-cost-retries-and-compaction).
  - `routing.selected` / `routing.fallback` — `{purpose, source, requested,
    model_id, model_name, provider, context_limit, fallback_reason,
    inference: "local"|"cloud", route: "vendor_cli"|"local_llamacpp"|"native_http"}`
    (the window shows `inference` as "· Cloud" / "· This computer").
  - `model.switched` `{provider, from, to, resumed}`; `vendor.session` `{vendor, session_id, job_id}`.
  - `usage.updated` — `{purpose, turn, job, session}` (per request) or
    `{vendor, usage}` (a vendor's plan-usage push; tell them apart by `vendor`).
  - `limit.reached` `{vendor, usage, detail, job_id}`; `limit.fallback` (see
    [plan limits](#plan-limits)).
  - `context.attached` `{path, success, origin: "explicit_file_request"|"nested_guidance"}`,
    `context.budget`, `context.compacted`, `autonomy.budget`.
  - `runaway.warning` `{kind?: "assistant_text"|"prose_command"|"redundant_observation", tool?, action: "warn"|"replan"|"pause", repeats, sample?}`.
  - `completion.retry` `{reason: "unperformed_action", attempt, max_attempts}`.
  - `local.runtime_progress` `{model_id, phase}`; `local.runtime_ready`
    `{model_id, runtime, preparation_seconds, comparison, automatic_cpu_fallback_allowed, request_policy}`
    (see [local runtime receipts](#local-runtime-receipts)).
- **Tools and approvals**
  - `tool.started` `{tool, arguments, call_id}`; `tool.completed` `{tool,
    success, output, error, call_id, output_preview, redacted?, sources?}`.
    `call_id` is a fresh opaque id per execution. The activity timeline
    classifies by `tool`: native names and vendor names such as
    `codex.command_execution`, `codex.file_change`, `cursor.read`, `Bash`, `Edit`.
  - `plan.updated` `{plan: {goal, steps: [{id, title, status, detail?}]}}`.
  - `web.source` `{url, final_url, title, status}`.
  - `files.changed` `{paths, detail?, vendor?}`.
  - `approval.requested` — the [Approval](#approvals) record (redacted);
    `approval.resolved` `{tool, approved, scope: "once"|"task", note?, call_id}`;
    `approval.granted` `{tool, call_id|job_id, grant}` (allowed without a
    prompt by an earlier "Allow for this task").
  - `hook.started` / `hook.completed` — the lifecycle command outcome.
  - `mcp.connected` `{server, name, hash, tools}`, `mcp.warning` `{server?, text}`, `mcp.closed` `{server, clean}`.
  - `verification.receipt`, `verification.retry` `{attempt, reason}`,
    `verification.summary` `{status, commands: [{command, exit_code, success, timed_out}], presented_as, note, vendor_agent, verified}`.
- **Checkpoints and review**: `checkpoint.updated` `{task_id, workspace,
  changes, paths, restored, source: "shell"|"vendor", changed,
  ignored_saved}` (`ignored_saved`: small Git-ignored files such as `.env` or
  a local SQLite database that the step deleted or changed, kept from before
  it so Rewind brings them back);
  `checkpoint.restored` `{task_id, paths, undo_id?}` (the transcript shows
  *Rewound to here · N files restored* above the task's prompt);
  `checkpoint.rewind_undone` `{task_id, paths, undo_id}`;
  `review.undone` `{task_id, path, hunk, whole}`.
- **Subagents** (on the parent conversation): `subagent.started` `{run_id,
  agent, description, prompt, mode, model, model_id, role, runner, vendor,
  route, cost, job_id, session_id, depth}`, `subagent.finished` `{run_id,
  agent, description, mode, model, model_id, role, runner, vendor, route,
  cost, status, summary, error, job_id, session_id, files, files_truncated,
  binary_files, patch, usage, steps, notes, verdict, duration_s}`,
  `subagent.applied` `{run_id, agent, role, paths}`.
- **Roles** (on a Plan → Implement → Review task): `roles.started` `{label,
  stages}` and `roles.finished` `{label, stages, applied, apply_note, files,
  completed}` ([Roles](#roles)).
- **Goals**: `goal.updated`, `goal.milestone.started` `{goal_id, milestone_id, job_id}`.
- **Automations** (on the run's conversation): `automation.started`
  `{automation_id, run_id, name, job_id}`, `automation.waiting`
  `{automation_id, run_id, name, notify, approval}`, `automation.finished`
  `{automation_id, run_id, name, status, notify, summary, detail}`.
- **Worktrees**: `worktree_task.closed` `{id, state, applied_files, branch}`
  (on the conversation); `worktree.created`, `worktree.removed`,
  `worktree.returned`, `worktree.repaired`, `worktree.changes_copied` (the
  record) and `worktree.restored` `{original_id, checkout}` (no conversation).
- **Background processes**: `background.started` `{id, name, command,
  workspace}`, `background.completed` `{id, name, workspace, status,
  exit_code, error, truncated}` (on the conversation that started them).
- **Extensions** (no conversation): `plugin.installation` `{workspace, removed, result}`,
  `mcp.registration` `{workspace, server, removed}`, `mcp.activation`
  `{workspace, server, hash, enabled}`, `hook.activation` `{workspace, path, hash, enabled}`.

### Transient broadcasts (never stored)

- `job.changed` `{job_id, status}` (with `session_id`, `task_id`): a job was
  queued or is cancelling.
- `agent.paused` / `agent.resumed` `{job_id, status}` when pause or resume is requested.
- `approval.expiring` `{approval_id, session_id, tool, command, expires_at,
  seconds_left}`: once, when 80% of a pending approval's time has passed
  (8 of the default 10 minutes; timeouts under a minute get none). The
  command is redacted.
- `account.login` `{vendor, line, url}` and `account.login.done` `{vendor,
  ok, detail, availability, availability_label}` (no conversation; read
  `GET /api/accounts/{vendor}/login`).
- `terminal.output` / `terminal.exited` `{terminal_id}`: at most one per 16 ms
  per terminal.
- `view.lagged`, `view.disconnected`, `view.reattached`: attached-view
  transport only.
- `second_opinion.updated` `{id, status, workspace, kind}` (with the
  record's `session_id`): a second opinion started, finished or stopped, or
  a finding changed. A wake-up only.

### Desktop notifications

The desktop shows a notification only while the window is unfocused or for a
conversation other than the one on screen (`shadowcode_core::notify`:
`select`, `should_show`, `hint`):

- `approval.requested` ("Waiting for you: <command or tool>"), `approval.expiring`,
  `agent.completed` with `success: false` and no plan limit ("task failed"),
  `limit.fallback` (and whether the task continued on a local model), and a
  successful `agent.completed`. Cancelled tasks never notify.
- Automation runs notify through `automation.finished` instead of their
  task's `agent.completed` (runs are recognized from `automation.started`),
  only when the automation's `notify` option is on: "ShadowCode · <name>"
  with the summary, or why it stopped (approval needed, time limit,
  failure). A run stopped by the user does not notify.
- Settings (`ui` group): `notify` (all), `notify_approval`, `notify_failed`,
  `notify_limit`, `notify_finished` (default on), `notify_sound` (default off).
- Tauri command `set_visible_session {sessionId}` tells the shell which
  conversation the window shows.

## Route index

Sorted by route (byte order), then method. **Stability**: `stable` or
`experimental` ([versioning policy](#versioning-policy)). **Remote**:

- `allowed` — served to paired remote devices (with the redactions in [Remote access](#remote-access)).
- `switch` — served remotely only while "Allow terminals over remote access" is on.
- `refused` — served to the desktop window and the local CLI; remote devices get 403.
- `local-only` — exists only on the local control socket (not in `Service::dispatch`,
  so neither IPC nor remote access serves it).

Some rows are platform-gated: `/api/preview/*` is Linux-only; `/api/remote*`,
`/api/plugins*`, `/api/mcp/*`, `/api/sqlite` and the routes that inspect the
project (`/api/doctor`, `/api/workspace/understand`, `/api/workspace/why`)
need a Unix build. ShadowCode 1.x ships for Linux, where all of them exist.

<!-- api-routes:begin -->
| Method | Route | Stability | Remote | Section |
| --- | --- | --- | --- | --- |
| `GET` | `/api/about` | stable | allowed | [About and updates](#about-and-updates) |
| `GET` | `/api/accounts` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/accounts/antigravity/install` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/accounts/antigravity/install` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/accounts/antigravity/uninstall` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/accounts/{vendor}/cancel-login` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/accounts/{vendor}/connect` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/accounts/{vendor}/disconnect` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/accounts/{vendor}/login` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/accounts/{vendor}/refresh` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/agents` | stable | allowed | [Subagents](#subagents) |
| `GET` | `/api/allowance` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/approvals` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `DELETE` | `/api/approvals/always` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/approvals/always` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/approvals/{id}` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/automations` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations/preview` | stable | allowed | [Automations](#automations) |
| `DELETE` | `/api/automations/{id}` | stable | allowed | [Automations](#automations) |
| `GET` | `/api/automations/{id}` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations/{id}` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations/{id}/pause` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations/{id}/resume` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations/{id}/run` | stable | allowed | [Automations](#automations) |
| `GET` | `/api/automations/{id}/runs` | stable | allowed | [Automations](#automations) |
| `POST` | `/api/automations/{id}/stop` | stable | allowed | [Automations](#automations) |
| `GET` | `/api/background` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/background` | stable | switch | [Terminals and background](#terminals-and-background) |
| `GET` | `/api/background/{id}` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/background/{id}/stop` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/checkpoints/rewinds/{undo_id}/undo` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `GET` | `/api/checkpoints/tasks/{task_id}` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `POST` | `/api/checkpoints/tasks/{task_id}/restore` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `GET` | `/api/cli-agents` | experimental | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/code-intel/config` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/embeddings/install` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/embeddings/remove` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/index/clear` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/index/focus` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/install` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/reindex` | stable | allowed | [Code intelligence](#code-intelligence) |
| `GET` | `/api/code-intel/repo-map` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/search` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/servers/stop` | stable | allowed | [Code intelligence](#code-intelligence) |
| `GET` | `/api/code-intel/status` | stable | allowed | [Code intelligence](#code-intelligence) |
| `POST` | `/api/code-intel/uninstall` | stable | allowed | [Code intelligence](#code-intelligence) |
| `GET` | `/api/commands` | stable | allowed | [Slash commands and memory](#slash-commands-and-memory) |
| `POST` | `/api/commands/run` | stable | allowed | [Slash commands and memory](#slash-commands-and-memory) |
| `POST` | `/api/compare` | stable | allowed | [Compare](#compare) |
| `GET` | `/api/compare/scoreboard` | stable | allowed | [Compare](#compare) |
| `GET` | `/api/compare/{id}` | stable | allowed | [Compare](#compare) |
| `POST` | `/api/compare/{id}/cancel` | stable | allowed | [Compare](#compare) |
| `POST` | `/api/compare/{id}/discard` | stable | allowed | [Compare](#compare) |
| `POST` | `/api/compare/{id}/keep` | stable | allowed | [Compare](#compare) |
| `POST` | `/api/compare/{id}/recover` | stable | allowed | [Compare](#compare) |
| `GET` | `/api/compares` | stable | allowed | [Compare](#compare) |
| `GET` | `/api/config` | stable | allowed | [Settings and health](#settings-and-health) |
| `PUT` | `/api/config` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/data` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `GET` | `/api/data/backups` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `POST` | `/api/data/backups` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `POST` | `/api/data/backups/inspect` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `DELETE` | `/api/data/pending` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `POST` | `/api/data/repair` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `POST` | `/api/data/reset` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `POST` | `/api/data/restore` | stable | refused | [Your data: backup, restore, repair, reset](#your-data-backup-restore-repair-reset) |
| `GET` | `/api/diagnostic-exports/{id}` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/doctor` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/events` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/feed` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/git` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/git/branch` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/git/pr` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/git/pr` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/git/pr/checks` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/git/push` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/git/suggest` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/goals` | stable | allowed | [Goals](#goals) |
| `POST` | `/api/goals` | stable | allowed | [Goals](#goals) |
| `DELETE` | `/api/goals/{id}` | stable | allowed | [Goals](#goals) |
| `GET` | `/api/goals/{id}` | stable | allowed | [Goals](#goals) |
| `POST` | `/api/goals/{id}/abandon` | stable | allowed | [Goals](#goals) |
| `POST` | `/api/goals/{id}/milestones/{milestone_id}` | stable | allowed | [Goals](#goals) |
| `POST` | `/api/goals/{id}/pause` | stable | allowed | [Goals](#goals) |
| `POST` | `/api/goals/{id}/run` | stable | allowed | [Goals](#goals) |
| `GET` | `/api/guardian` | stable | allowed | [Settings and health](#settings-and-health) |
| `POST` | `/api/guardian/approve-patch` | experimental | allowed | [Settings and health](#settings-and-health) |
| `POST` | `/api/guardian/request-patch` | experimental | allowed | [Settings and health](#settings-and-health) |
| `POST` | `/api/guardian/run` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/health` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/hooks` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/hooks/activation` | stable | allowed | [Extensions](#extensions) |
| `GET` | `/api/issues` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/issues/{number}` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/jobs` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/jobs/current` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/test` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/verification-refresh` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/jobs/{id}` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/cancel` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/jobs/{id}/events` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/note_edit` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/pause` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/resume` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/rewind` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/spending` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/jobs/{id}/steer` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/jobs/{id}/verification` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/local-models` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/add` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/local-models/downloads` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/downloads/cancel` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/downloads/delete` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/downloads/pause` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/downloads/start` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/import-ollama` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/load` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/remove` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/local-models/unload` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/logs` | stable | refused | [Settings and health](#settings-and-health) |
| `POST` | `/api/logs/folder` | stable | refused | [Settings and health](#settings-and-health) |
| `POST` | `/api/mcp/activation` | stable | allowed | [Extensions](#extensions) |
| `GET` | `/api/mcp/servers` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/mcp/servers` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/mcp/servers/delete` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/memory` | stable | allowed | [Slash commands and memory](#slash-commands-and-memory) |
| `GET` | `/api/models` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/models/register` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/models/select` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/models/test` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/onboarding` | stable | allowed | [Settings and health](#settings-and-health) |
| `POST` | `/api/onboarding` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/openrouter` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/openrouter/key` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/openrouter/refresh` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/owned-jobs` | stable | local-only | [Local control socket](#local-control-socket) |
| `GET` | `/api/parallel` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/parallel/cleanup` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/parallel/prepare` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/parallel/verify` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/parallel/worker-status` | stable | allowed | [Worktrees](#worktrees) |
| `GET` | `/api/picker` | stable | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/plugins` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/plugins/install` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/plugins/preview` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/plugins/remove` | stable | allowed | [Extensions](#extensions) |
| `POST` | `/api/preview/open` | stable | refused | [Preview](#preview) |
| `GET` | `/api/preview/servers` | stable | refused | [Preview](#preview) |
| `GET` | `/api/projects` | stable | allowed | [Conversations](#conversations) |
| `POST` | `/api/projects` | stable | allowed | [Conversations](#conversations) |
| `POST` | `/api/projects/trust` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/providers` | experimental | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/providers/detect` | experimental | allowed | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/remote` | stable | refused | [Remote access](#remote-access) |
| `PUT` | `/api/remote` | stable | refused | [Remote access](#remote-access) |
| `POST` | `/api/remote/devices/revoke` | stable | refused | [Remote access](#remote-access) |
| `PUT` | `/api/remote/ntfy` | stable | refused | [Remote access](#remote-access) |
| `POST` | `/api/remote/ntfy/test` | stable | refused | [Remote access](#remote-access) |
| `POST` | `/api/remote/pair` | stable | refused | [Remote access](#remote-access) |
| `GET` | `/api/resolve` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/review/tasks/{task_id}` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `POST` | `/api/review/tasks/{task_id}/explain` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `GET` | `/api/review/tasks/{task_id}/file` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `POST` | `/api/review/tasks/{task_id}/undo` | stable | allowed | [Review and rewind](#review-and-rewind) |
| `GET` | `/api/roles` | stable | allowed | [Subagents](#subagents) |
| `POST` | `/api/roles` | stable | allowed | [Subagents](#subagents) |
| `GET` | `/api/routing` | stable | allowed | [Settings and health](#settings-and-health) |
| `PUT` | `/api/routing` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/rules` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `GET` | `/api/rules/check` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `GET` | `/api/rules/export` | stable | refused | [Rules and skills](#rules-and-skills) |
| `DELETE` | `/api/rules/export/{target}` | stable | refused | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/export/{target}` | stable | refused | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/folder` | stable | refused | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/imports` | stable | refused | [Rules and skills](#rules-and-skills) |
| `DELETE` | `/api/rules/imports/{name}` | stable | refused | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/imports/{name}/update` | stable | refused | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/items` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `GET` | `/api/rules/preview` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `PUT` | `/api/rules/profile` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/sharing` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `GET` | `/api/rules/starters` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/rules/starters` | stable | allowed | [Rules and skills](#rules-and-skills) |
| `POST` | `/api/run` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/runtime` | stable | local-only | [Local control socket](#local-control-socket) |
| `POST` | `/api/sandbox/discard-scratch` | experimental | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/sandbox/status` | stable | allowed | [Settings and health](#settings-and-health) |
| `GET` | `/api/second-opinions` | stable | allowed | [Second opinions](#second-opinions) |
| `POST` | `/api/second-opinions` | stable | allowed | [Second opinions](#second-opinions) |
| `GET` | `/api/second-opinions/current` | stable | allowed | [Second opinions](#second-opinions) |
| `GET` | `/api/second-opinions/options` | stable | allowed | [Second opinions](#second-opinions) |
| `POST` | `/api/second-opinions/prefs` | stable | allowed | [Second opinions](#second-opinions) |
| `GET` | `/api/second-opinions/{id}` | stable | allowed | [Second opinions](#second-opinions) |
| `POST` | `/api/second-opinions/{id}/cancel` | stable | allowed | [Second opinions](#second-opinions) |
| `POST` | `/api/second-opinions/{id}/findings/{finding}` | stable | allowed | [Second opinions](#second-opinions) |
| `POST` | `/api/second-opinions/{id}/findings/{finding}/fix` | stable | allowed | [Second opinions](#second-opinions) |
| `GET` | `/api/secrets` | stable | refused | [Accounts and models](#accounts-and-models) |
| `POST` | `/api/secrets/move` | stable | refused | [Accounts and models](#accounts-and-models) |
| `GET` | `/api/sessions` | stable | allowed | [Conversations](#conversations) |
| `POST` | `/api/sessions` | stable | allowed | [Conversations](#conversations) |
| `DELETE` | `/api/sessions/{id}` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/sessions/{id}` | stable | allowed | [Conversations](#conversations) |
| `PATCH` | `/api/sessions/{id}` | stable | allowed | [Conversations](#conversations) |
| `POST` | `/api/sessions/{id}/activate` | stable | allowed | [Conversations](#conversations) |
| `POST` | `/api/sessions/{id}/branch` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/sessions/{id}/cost` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/sessions/{id}/events` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/sessions/{id}/export` | stable | allowed | [Conversations](#conversations) |
| `POST` | `/api/sessions/{id}/fork` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/sessions/{id}/pins` | experimental | allowed | [Conversations](#conversations) |
| `POST` | `/api/sessions/{id}/pins` | experimental | allowed | [Conversations](#conversations) |
| `DELETE` | `/api/sessions/{id}/pins/{pin_id}` | experimental | allowed | [Conversations](#conversations) |
| `DELETE` | `/api/sessions/{id}/scheduled-resume` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/sessions/{id}/scheduled-resume` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/sessions/{id}/scheduled-resume` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/sessions/{id}/target` | stable | allowed | [Conversations](#conversations) |
| `GET` | `/api/spending` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `GET` | `/api/spending/estimate` | stable | allowed | [Jobs and approvals](#jobs-and-approvals) |
| `POST` | `/api/sqlite` | stable | allowed | [Extensions](#extensions) |
| `GET` | `/api/subagents` | stable | allowed | [Subagents](#subagents) |
| `GET` | `/api/subagents/{id}` | stable | allowed | [Subagents](#subagents) |
| `GET` | `/api/terminals` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/terminals` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/terminals/{id}/close` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/terminals/{id}/input` | stable | switch | [Terminals and background](#terminals-and-background) |
| `GET` | `/api/terminals/{id}/output` | stable | switch | [Terminals and background](#terminals-and-background) |
| `POST` | `/api/terminals/{id}/resize` | stable | switch | [Terminals and background](#terminals-and-background) |
| `GET` | `/api/updates` | stable | allowed | [About and updates](#about-and-updates) |
| `POST` | `/api/updates/check` | stable | allowed | [About and updates](#about-and-updates) |
| `POST` | `/api/updates/dismiss` | stable | allowed | [About and updates](#about-and-updates) |
| `GET` | `/api/version` | stable | allowed | [Settings and health](#settings-and-health) |
| `POST` | `/api/views` | stable | local-only | [Local control socket](#local-control-socket) |
| `POST` | `/api/voice/cancel` | stable | refused | [Voice](#voice) |
| `POST` | `/api/voice/config` | stable | allowed | [Voice](#voice) |
| `POST` | `/api/voice/models/install` | stable | allowed | [Voice](#voice) |
| `POST` | `/api/voice/models/remove` | stable | allowed | [Voice](#voice) |
| `GET` | `/api/voice/recording` | stable | refused | [Voice](#voice) |
| `POST` | `/api/voice/start` | stable | refused | [Voice](#voice) |
| `GET` | `/api/voice/status` | stable | allowed | [Voice](#voice) |
| `POST` | `/api/voice/stop` | stable | refused | [Voice](#voice) |
| `POST` | `/api/voice/transcribe` | stable | allowed | [Voice](#voice) |
| `POST` | `/api/workspace/attach` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `POST` | `/api/workspace/attach-image` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `POST` | `/api/workspace/context-preview` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/diff` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/diff/hunk` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/diffstat` | stable | allowed | [Git and forge](#git-and-forge) |
| `DELETE` | `/api/workspace/editor-draft` | stable | refused | [Workspace and files](#workspace-and-files) |
| `PUT` | `/api/workspace/editor-draft` | stable | refused | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/editor-drafts` | stable | refused | [Workspace and files](#workspace-and-files) |
| `POST` | `/api/workspace/exec` | stable | switch | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/file` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `PUT` | `/api/workspace/file` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/files` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/git` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/git/add` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/git/commit` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/workspace/git/hooks` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/git/hooks` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/git/ignore` | stable | allowed | [Git and forge](#git-and-forge) |
| `POST` | `/api/workspace/git/unstage` | stable | allowed | [Git and forge](#git-and-forge) |
| `GET` | `/api/workspace/instructions` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `PUT` | `/api/workspace/instructions` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/mentions` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/skills` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `PUT` | `/api/workspace/skills` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/status` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/understand` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `POST` | `/api/workspace/understand` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/workspace/why` | stable | allowed | [Workspace and files](#workspace-and-files) |
| `GET` | `/api/worktree-tasks` | stable | allowed | [Worktrees](#worktrees) |
| `GET` | `/api/worktree-tasks/setup` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktree-tasks/setup` | stable | allowed | [Worktrees](#worktrees) |
| `GET` | `/api/worktree-tasks/{id}` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktree-tasks/{id}/apply` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktree-tasks/{id}/discard` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktree-tasks/{id}/keep-branch` | stable | allowed | [Worktrees](#worktrees) |
| `GET` | `/api/worktrees` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/copy-changes` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/inspect` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/recovery` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/remove` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/repair` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/restore` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/return` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/review-changes` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/review-repair` | stable | allowed | [Worktrees](#worktrees) |
| `POST` | `/api/worktrees/review-return` | stable | allowed | [Worktrees](#worktrees) |
<!-- api-routes:end -->

## Conversations

A conversation (`session`) belongs to one project folder and holds tasks,
their events, and a model message tape.

```
Session = {id, workspace, title, status, created_at, updated_at, model_id,
           usage_json, parent_id, branched_at}
SessionRow = Session + {compare_id, compare_lane, worktree_task, worktree_source, subagent_parent}  // each string|null
```

- `GET /api/sessions?q=&workspace=&limit=&include_compare=&include_subagents=`
  → `{sessions: SessionRow[]}`, newest `updated_at` first. `q` matches the
  title, folder or any task prompt. `limit` default 100, max 10 000.
  Compare lanes and subagent conversations are hidden unless
  `include_compare=true|1` / `include_subagents=true|1`. `workspace=<project>`
  also lists conversations running in that project's worktree tasks.
- `POST /api/sessions {workspace?, title?}` → the new `Session` (+ `usage`);
  selects it. `workspace` defaults to the selected project.
- `GET /api/sessions/{id}?summary=true` → `Session` + `usage: Usage`.
- `GET /api/sessions/{id}[?view=window]` →
  ```
  Session + {
    usage: Usage,                         // sum of finished jobs
    tasks: [{id, session_id, prompt, status, summary, created_at, completed_at, usage_json}],  // newest first; [] with view=window
    events: EventRow[],                   // view=window: the newest history page; otherwise up to 10 000 newest
    history_page?: {first_cursor, has_older},  // view=window only
    event_cursor: number,
    execution_target: string|null,        // picker id remembered for this conversation
    native_sessions: {[vendor]: string},  // vendor CLI session ids
    compare_id: string|null, compare_lane: string|null,
    worktree: WorktreeTask|null,          // as last saved
    subagent_parent: string|null, subagent_run: string|null
  }
  ```
- `POST /api/sessions/{id}/activate[?view=window]` → the same as `GET`, and
  selects the conversation and its project.
- `PATCH /api/sessions/{id} {title}` → `{ok: true}`.
- `DELETE /api/sessions/{id}` → `{ok: true}`. Also deletes its subagent
  conversations (any depth), their run records and saved patches; a subagent
  conversation that a fork still shows moves to that fork. Refused while a
  goal runs in it ("Pause the goal before deleting its session"), during a
  manual operation in its folder, and while the conversation still has its
  worktree task.
- `POST /api/sessions/{id}/target {target_id}` → `{ok, execution_target,
  provider, applies_to: "this_turn"|"next_turn"}`. `target_id` (1–1024 bytes)
  must be a picker id the backend resolves. Remembered per conversation and
  as the project default (`native_meta` `execution_target:<workspace>`). A
  running job keeps its target: `next_turn` means the switch applies to the
  next turn. `execution_target` may already hold the project default for a
  conversation without its own; the window otherwise falls back to the last
  target chosen in that project, never to `config.model`.
- `POST /api/sessions/{id}/branch {title?}` → the new `Session` (`parent_id`
  set; title defaults to "<title> (branch)"). Copies all events, pins, the
  latest message tape and the task notes as inherited notes.
- `POST /api/sessions/{id}/fork {event_id, title?, before?}` → `{fork: Session,
  original: Session, forked_from_event: number|null, original_intact: true}`.
  `event_id` is required (an integer). With `before: true` the fork keeps
  only the events before the task `event_id` belongs to (Edit & resend);
  before the first event the fork is an empty conversation with `parent_id` set.
- `GET /api/sessions/{id}/events` — three modes:
  - `?view=window[&before=<cursor>]` → `{events, first_cursor, event_cursor,
    has_older}`: at most 128 events and 2 MB of payload, ending at the
    conversation's cursor or just before `before`.
  - `?before=<cursor>&limit=` → `{events}`: the page ending just before
    `before` (default 256, max 2000).
  - `?after=<cursor>&limit=` → `{events}`: events after `after`, oldest first
    (default 512, effective max 2000).
  `before` must be a positive integer ("Invalid history cursor").
- `GET /api/sessions/{id}/export?format=md|json` → `{filename, content, mime}`
  (`shadowcode-<id>.md`, `text/markdown`, or `.json`, `application/json`).
  JSON is the session with every event, tasks, `task_notes` and
  `inherited_notes`. Refused above 32 MB. The desktop command
  `export_session {sessionId, format}` saves it through a file dialog.
- `GET /api/sessions/{id}/cost` → `{session_id, tasks: [{task_id, prompt,
  status, usage}], usage, cost, cost_estimated, note}`.
- `GET /api/sessions/{id}/pins` → `{pins: [{id, session_id, task_id, ts,
  label, body}]}`; `POST /api/sessions/{id}/pins {label, body}` → `{id}`;
  `DELETE /api/sessions/{id}/pins/{pin_id}` → `{ok: true}`. (`/pin` creates
  pins; experimental.)
- `GET /api/events?session_id=&limit=` → `{events}`: the newest events of that
  conversation (default the selected one), oldest first; default 240, max 10 000.
- `GET /api/resolve?kind=session|job&prefix=` → `{id}`: the one id starting
  with `prefix` (1–128 of `[A-Za-z0-9_-]`); ambiguous or unknown prefixes fail.

Projects:

- `GET /api/projects` → `{projects: [{id, path, name, last_opened}]}`, most
  recent first (at most 1000). Temporary worktrees are never listed.
- `POST /api/projects {path}` → for a trusted folder `{ok: true,
  needs_trust: false, path, session_id}` (a new conversation titled with the
  folder name, selected; the desktop also remembers the folder for its next
  start); for an untrusted one `{needs_trust: true, path, name, permissions}`.
- `POST /api/projects/trust {path}` adds the folder to `trusted_workspaces`,
  then answers like `POST /api/projects`.

## Jobs and approvals

A **job** runs one task (an agent turn, a command check or a workflow) in a
conversation. One job runs per checkout at a time; others queue.

```
Job = {id, workspace, session_id, task_id, task, images: string[], web,
       status: "queued"|"running"|"paused"|"cancelling"|"completed"|"failed"|"cancelled"|"limit_reached"|"interrupted",
       mode: "code"|"plan"|"review"|"command", model,
       routing: RoutingDecision|null, workflow: {name, kind, source, path, hash, mode}|null,
       started_at, finished_at: number|null, event_cursor, summary,
       usage: Usage, usage_is_estimated, steps,
       result: {success, cancelled, summary, plan, usage, usage_is_estimated, verification, timings, limit_reached?, command?}|null,
       timings: Timings|null}
RoutingDecision = {purpose, source, requested, model_id, model_name, provider, context_limit,
                   fallback_reason: string|null, inference: "local"|"cloud", route: "vendor_cli"|"local_llamacpp"|"native_http"}
JobSummary = {id, workspace, session_id, task_id, status, mode, purpose, model (≤512), started_at, finished_at,
              event_cursor, task (≤512 chars), task_truncated}
```

`interrupted`: the application stopped before the job finished (set at the
next start). `limit_reached`: a vendor reported its plan limit; the job stopped
without retry and `result.limit_reached = {vendor, detail, usage}`.
ShadowCode never buys credits, redeems resets or enables overages.

A vendor turn that fails, reaches the plan limit or is cancelled keeps the
tokens and cost the vendor had already reported: they are in `job.usage`,
`result.usage` and the session total, with a `usage.updated` event.

A managed local model that could not allocate memory while loading ends the
job `failed` with a plain summary and `result.local_out_of_memory =
{model_id, model, context_tokens, memory: "gpu"|"system", cpu_tried,
smaller_context: number|null, detail}`. On an ordinary task a GPU shortage is
retried once on the CPU instead (`loaded.fallback_out_of_memory`, plus
`agent.warning {kind: "local_memory"}`); Compare lanes never fall back.

### Starting a task

`POST /api/jobs` (alias `POST /api/run`) → `Job` (+ `worktree_task`), or the
in-band consent refusal ([Errors](#errors)). Body:

| Field | Type | Meaning |
| --- | --- | --- |
| `task` | string | the request text |
| `workspace` | string? | project; default the selected one. Must be trusted ("Trust this project before starting an agent task"). |
| `session_id` | string? | conversation; default the selected one when it is in this project, else a new one |
| `model` | string? | exact picker id; stored as the conversation's `execution_target` and the project default. Without it: the conversation's target, then the project default, then `config.model.default`. |
| `purpose` | string? | `planner`/`plan`/`planning`/`researcher`/`architecture` → Plan (read-only); `reviewer`/`review` → Ask (read-only review); anything else → Code |
| `queue` | bool? | queue behind the project's active job; without it a busy project refuses ("This workspace has an active task. Queue a follow-up or stop the current task first.") |
| `images` | string[]? | attachment paths from `POST /api/workspace/attach-image`; at most 4. Refused before a job exists for a local row without vision. |
| `web` | bool? | offer `web_fetch`/`web_search` (native loop, `network.mode: online` only); echoed as `Job.web` |
| `handoff_consent` | bool? | the user agreed to hand the conversation to a cloud route (below) |
| `effort` | string? | `low`/`medium`/`high`; `""`/`"default"` keeps the model's own; anything else is refused |
| `mentions` | `[{path, kind: "file"\|"dir"}]`? | at most 20, inside the project (strict) |
| `context` | `[{kind: "element"\|"console", label, text}]`? | from the Preview tab; at most 12 items of 16 000 characters (strict; other fields ignored) |
| `permission_limit` | `"read_only"\|"workspace"\|"elevated"`? | narrows the project's permission level for this job (strict) |
| `worktree` | bool? | start a new conversation in a fresh worktree ([Worktree tasks](#worktree-tasks)); `session_id` and `queue` are ignored |
| `base_branch` | string? | with `worktree`: start from this local branch instead of the current files |
| `only_change` | bool? | "Only change these": ShadowCode's own agent asks before any file tool changes a path outside `mentions` (even when edits are allowed; approval `reason` "Outside the files you chose for this task: …", with its own `grant` "file edits outside the files you chose", so allowing file edits for the task never covers them); a native `exec` that changed paths outside them reports `scope.outside {paths, tool: "exec"}` after it ran (and `outside_scope` in its result); a command that can write and ran without a project checkpoint (`checkpoints.shell` off, or it was unavailable or failed) is not checked: `agent.warning {kind: "scope"}` says so once per turn, and its result carries `scope_unchecked {reason, note}`; after a subscription turn, changed paths outside them, including ones too large to record, are reported as `scope.outside {job_id, paths}`, or `agent.warning {kind: "scope"}` says the turn could not be checked (no project checkpoint: `checkpoints.vendor` off, or it was unavailable or failed). `background_start` processes are not checked |
| `roles` | bool? | run a Code task as Plan → Implement → Review, a Plan task as its plan role ([Roles](#roles)); other modes are refused |

- Job records keep the exact picker id in `routing.model_id`. A `local:gguf:`
  target also becomes the project's "last local model" (plan-limit fallback).
- **Mentions.** Native models read each mentioned file's current text
  (64 KiB each, 256 KiB in all) or folder listing with the prompt; the stored
  prompt and `user.message` keep only the text (vendor CLIs resolve the
  `@path` in it).
- **Preview context** is appended after `task` under a line saying it was
  captured from the page and is data, not instructions, for every runner; the
  stored prompt and `user.message` include it. See [PREVIEW.md](PREVIEW.md).
- **Effort** maps to OpenRouter `reasoning.effort`, llama.cpp
  `chat_template_kwargs.enable_thinking` (false for `low`) on templates with
  the switch, Codex `turn/start.effort` (and `-c model_reasoning_effort="…"`
  for new threads), Claude Code `--effort <level>` (`MAX_THINKING_TOKENS`
  4 000 / 16 000 / 31 999 only for a Claude Code without `--effort`) and an
  ACP session's `thought_level` option (Grok `reasoning_effort`); other
  runtimes ignore it. Picker rows say whether it applies (`reasoning`).
- **Offline** (`network.mode: offline`): jobs on cloud routes are refused with
  "Offline mode: choose a model that runs on this computer"; loopback
  endpoints and managed llama.cpp still run. `api:openrouter:` jobs are also
  refused before a job exists when no key is stored.
- **Handoff consent.** Required when the target is a cloud route and (a) the
  previous turn ran on this computer, (b) the previous turn ran on a
  different provider (its unseen turns are handed over), or (c) the request
  attaches images and this conversation never consented to cloud
  attachments. Without consent nothing is written and the answer is
  `{ok: false, status: 409, error, needs_consent: true, handoff: {from, to, excerpt_chars, images, reason}}`.
- **Handoff.** When the provider changes, the turns the new provider has not
  seen (user requests, final answers, changed files; at most 12 000
  characters) are sent as a `<prior_conversation>` block marked as context,
  not instructions: before the task text for vendor CLIs; in the message tape
  for the native loop (vendor turns are appended to it). An `agent.handoff`
  event records it. Same provider, different model: no handoff; the vendor
  switches the model on the resumed session and `model.switched` is emitted.
- **Read-only vendor tasks** (Plan/Ask): command and file-change prompts from
  the vendor are denied automatically with an `agent.warning` ("Denied
  automatically: …"). Every vendor sends permission requests (Antigravity
  through its ACP agent server). Antigravity questions sent through the
  permission channel (`interaction_` tool call ids) are cancelled with an
  `agent.warning` telling the user to answer in the next message.
- The desktop window handles `/model` itself (it opens the picker).

### Command checks

- `POST /api/jobs/test {workspace?, session_id?, command?, timeout?, queue?}`
  → `Job` (`mode: "command"`). Runs `command` (default: the project's
  detected test command) against current files; `timeout` seconds, default
  300 (a shorter configured tool timeout wins). `workspace` must be the
  selected project ("Test task belongs to another workspace"). No model is
  involved; trust, command permissions and approvals apply. The desktop's
  **Run a check…** sends `{workspace, session_id, command, timeout: 300,
  queue: false}` with a command the user typed; receipt text is never
  replayed as input. The composer draft and earlier receipts are kept;
  workspace, conversation and navigation guards stop a late answer from
  attaching to another conversation; a refresh failure after acceptance does
  not make the command retryable. Output and the new receipt appear in the
  ordinary transcript. A successful exit verifies that check only.
- `GET /api/jobs/{id}/verification` → the current `Verification` assessment
  of that job's receipts against the files now on disk.
- `POST /api/jobs/verification-refresh {job_ids: string[]}` (strict) →
  `{verifications: {[job_id]: Verification}}`. 1–32 unique non-empty ids of at
  most 128 bytes, all existing jobs; any invalid id rejects the whole request.
  One fingerprint per workspace is shared within the batch; nothing is cached
  across requests. Unreadable or missing content cannot keep a passing
  verdict; stored receipts are not changed; vendor-owned and non-passing
  receipts need no scan. The window refreshes visible summaries every 5 s
  plus on edits and engine wake-ups; hidden cards do not poll and pending
  reads never overlap. These are point-in-time observations with bounded
  delay, not a filesystem watch or a claim that nothing changed afterwards.

### Reading and controlling jobs

- `GET /api/jobs?view=summary&limit=` → `{jobs: JobSummary[]}`: the newest
  `limit` (default and max 100) plus every active job. Without
  `view=summary` → `{jobs: Job[]}` (active and recent, up to 1000).
- `GET /api/jobs/current?session_id=&include_finished=true` → `{job: Job|null}`:
  the running or paused job, else a cancelling one, else the oldest queued
  one; with `include_finished` the most recently finished one last.
- `GET /api/jobs/{id}` → `Job`, with `usage` and `timings` observed now.
- `GET /api/jobs/{id}/events?after=&limit=` → `{events, job}` (default 512,
  max 2000). A finished job's events stop at its final cursor.
- `POST /api/jobs/{id}/cancel {only_if_queued?}` → `Job`. With
  `only_if_queued` it fails once the job has left the queue.
- `POST /api/jobs/{id}/pause`, `.../resume` → `Job` (running jobs only).
- `POST /api/jobs/{id}/steer {instruction, path?}` → `Job`: steering for the
  next step of a running job.
- `POST /api/jobs/{id}/note_edit {path, detail?}` → `Job`: tells a running
  job the user edited `path`.
- `POST /api/jobs/{id}/rewind` → `{ok, job_id, task_id, session_id, restored:
  string[], note}`: restores the files the job changed (file tools, shell
  commands and subscription turns). Refused for a running vendor job
  ("Wait for the subscription turn to finish, then rewind…"). Writes
  `checkpoint.restored`; after a finished task is restored the next turn also
  sees a process note that the edits are no longer on disk.

### Approvals

```
Approval = {id, session_id, task_id, tool, arguments, command, reason, pending,
            created_at, expires_at, preview: Preview|null, grant: string, note: boolean,
            assessment: Assessment|null, always: string}
Assessment = {risk: "read_only"|"changes_files"|"network"|"outside"|"destructive"|"remote_code"|"admin",
              risk_label, explanation, undo: "nothing"|"yes"|"partly"|"no", undo_label,
              notes: string[], read: boolean, checks: [{title, level?: "info"|"warn"|"danger", items: string[]}]}
Preview = {kind: "files", files: [{path, status: "added"|"modified"|"deleted", diff, added, removed, truncated, binary}]}
        | {kind: "command", command, cwd} | {kind: "move", from, to} | {kind: "folder", path}
```

- `preview.files[].diff` is a unified diff body against the file as it is now
  (at most 2 000 lines per file and 256 KB per prompt; later files keep their
  counts with `truncated`). Vendor prompts carry previews where the protocol
  describes the change (Claude `Write`/`Edit`/`MultiEdit`, ACP `diff`
  content, Codex `applyPatchApproval`; commands with their `cwd`).
- `grant` says what "Allow for this task" covers ("file edits", "`cargo test`
  commands", "`server / tool` calls"); empty when the action can only be
  allowed once (chained, redirected, privileged, deleting or
  history-rewriting commands; extra sandbox permissions).
- `note`: a deny note reaches the agent (native tools and Claude Code).
- `assessment` (since 1.0) says what the action does in one plain sentence,
  its risk tag and whether Rewind can undo it, for ShadowCode's own tools and
  for vendor requests alike. Shell commands are parsed with tree-sitter-bash
  (pipelines, lists, subshells, substitutions, heredocs, redirects, `sudo` /
  `env` / `timeout` wrappers and `bash -c` / `eval` text); the riskiest step
  decides the tag. `read: false` means part of the command could not be read
  ahead (a syntax error, a path or program from a variable, code built while
  it runs). `checks` is where other reviews of the same action add a section.
- `always` (since 1.0) says what "Always allow in this project" would cover
  ("Always allow `cargo test` in this project"); empty unless the command is
  one exact, fully read test, build, lint or type-check command with no
  redirects to files, variables, paths outside the project, installs or
  network use.
- `reason` in `ask` mode reads `Write <path>`, `Edit <path>`, `Create
  directory <path>`, `Move <a> to <b>`, `Delete <path>`, `Apply a patch to <files>`.
- Native approvals expire after 10 minutes; vendor approvals after
  `cli_agents.approval_timeout_sec` (default 600, 10–86 400). An expired
  approval is denied.

Routes:

- `GET /api/approvals?session_id=` → `{approvals: Approval[]}` (pending; all
  conversations without `session_id`).
- `POST /api/approvals/{id} {decision: "approve"|"deny", session_id?, scope?:
  "once"|"task"|"project", note?}` → the answered `Approval`. `scope:
  "project"` with `approve` (only when `always` is set) stores the command in
  the project's rules; later requests of exactly that command, from
  ShadowCode's own agent or a vendor CLI, run without a prompt and are
  recorded as `approval.granted {…, scope: "project", command}`. Each rule is
  checked again before use. `session_id` defaults to
  the selected conversation. `scope: "task"` with `approve` keeps a grant
  until the task ends: later requests of the same task with the same scope
  (tool kind, or the same program and subcommand for commands) are allowed
  without a prompt and recorded as `approval.granted`. A scope the prompt
  does not offer is refused. For Codex the first grant answers
  `acceptForSession` / `approved_for_session`; other vendors receive single
  allows. `note` (with `deny`, at most 2 000 bytes) becomes the tool error
  the model reads ("The user denied this action and said: …") or Claude's
  denial message; `approval.resolved` carries `scope` and `note`.
- `GET /api/approvals/always?workspace=` → `{workspace, commands: [{command,
  added_at}]}`: the project's "Always allow" commands (default: the selected
  project). Stored in ShadowCode's database (`native_meta`
  `always_allow:<project>`), never in the repository; at most 100.
- `DELETE /api/approvals/always {workspace?, command}` → the same, without it.

### Feed

- `GET /api/feed?session_id=&limit=` → `{approvals: Approval[], jobs:
  JobSummary[], events: string[], waiting: string[]}`. `approvals` are the
  pending approvals of that conversation (all without `session_id`); `jobs`
  are the rows of `GET /api/jobs?view=summary`; `waiting` lists every
  conversation with a pending approval (sidebar badges); `events` lists the
  broadcast types after which the feed may have changed: `approval.requested`,
  `approval.resolved`, `job.changed`, `agent.started`, `agent.completed`,
  `agent.paused`, `agent.resumed`, `limit.fallback`. The window reads the feed
  on those wake-ups plus a 15 s backstop.

### Plan limits

Config `limits: {on_limit: "local"|"ask", fallback_model: ""|"local:gguf:…"}`
(default `local`). When a vendor job ends `limit_reached`, the engine records
`limit.fallback` on that task:

- `{ok: true, from, to, target, job_id}`: a follow-up job started in the same
  conversation on local model `target` with the task "Continue where <from>
  stopped when its plan limit was reached. The request was: …"; the
  conversation's `execution_target` becomes `target`.
- `{ok: false, ask: true}`: `on_limit` is `ask`; nothing started.
- `{ok: false, from, reason}`: no local model is ready.

The fallback model is `fallback_model` when ready, else the last local model
used in the project, else the first ready local model with tool support.

Event `limit.reached {vendor, usage, detail, job_id, resets_at}` is recorded
on the limited task when the vendor stops the turn; the job's
`result.limit_reached` carries the same `resets_at`. `resets_at` (Unix
seconds or null) is when the plan resets: the latest reset among the vendor's
exhausted usage windows, else a time in the vendor's error text ("try again
at 3:40 PM", "resets in 2h 5m", "try again at Oct 1st, 2026 3:40 PM", an RFC
3339 time, Claude's `…|<unix seconds>`; read in this computer's time zone
unless it says UTC), else the earliest reported window reset. Times in the
past or more than 8 days away are not believed.

### Resume after a plan limit

A one-shot continuation on the same model when the plan resets. It is saved
in `native_meta` `scheduled_resumes` (one per conversation), survives a
restart, and is started by the automation scheduler (desktop and `shadowcode
serve`; one-shot CLI commands never run it).

- `GET /api/sessions/{id}/scheduled-resume` → `{resume: Resume|null,
  scheduler: bool}`; `scheduler` is false when this engine does not run
  schedules.
- `POST /api/sessions/{id}/scheduled-resume {job_id?, handoff_consent?}` →
  `{resume, scheduler}`. `job_id` (default: the conversation's latest job)
  must be a job of this conversation with status `limit_reached` and a
  future `resets_at`; otherwise 400. Replaces the conversation's earlier
  schedule.
- `DELETE /api/sessions/{id}/scheduled-resume` → `{resume: Resume|null}` (the
  removed one).
- `Resume = {id, session_id, workspace, job_id, task_id, target, label, task,
  mode, web, at, created_at, handoff_consent}`: `target` is the limited job's
  exact picker id (`routing.model_id`), `label` its product ("Codex").
- At `at` the scheduler starts a queued job in the same conversation on
  `target` with the task "Continue where <label> stopped when its plan limit
  was reached. The request was: …", and sets the conversation's
  `execution_target` to `target`. It never switches to another model: when
  `target` cannot be resolved or started, nothing runs and the conversation
  says why.
- Events on the limited task (`resume_id, at, target, label, job_id` in each):
  `resume.scheduled {scheduler}`, `resume.cancelled`, `resume.started`
  (`job_id` is the new job), `resume.missed` (ShadowCode was not running and
  the time is more than 12 hours past), `resume.failed {reason, task}`, and
  `resume.needs_consent {reason, task}` (continuing would hand newer turns to
  a cloud route; the window starts `task` on `target` through the usual
  consent dialog).

### Usage, cost, retries and compaction

One `Usage` shape is used for turns, jobs and conversations:

```
Usage = {prompt_tokens, completion_tokens, total_tokens,
         cached_tokens,        // input served from the provider's cache (included in prompt_tokens)
         cache_write_tokens,
         cost_usd: number|null, cost_estimated: boolean,
         estimated: boolean,   // some token counts were estimated by ShadowCode
         source: "provider"|"local"|"vendor"|"mixed"|"",
         turns: number}
```

- Cost comes from OpenRouter's `usage.cost` (requested with `usage: {include:
  true}`); `0` for a model on this computer (llama.cpp, Ollama, loopback
  servers); OpenRouter's cached per-token prices when a turn reported no cost
  (`cost_estimated: true`; cached input priced as full input, an upper
  bound); otherwise `null`. Subscription jobs carry what the vendor reports
  with `source: "vendor"`: token counts (Claude, Codex, ACP), cached input
  and Claude's `total_cost_usd`. A vendor that reports no counts gives
  `estimated: true` and zeros. `usage_is_estimated` is kept for older readers.
- `usage.updated {purpose, turn, job, session}` after every counted request:
  `purpose` is `turn`, `compaction`, `vendor` (a finished subscription turn),
  `failed_attempt` (tokens the provider reported for a failed request) or
  `subagent` (a finished subagent's whole usage, added to its parent job);
  `session` includes the running job.
- `model.retry {attempt, max_attempts, reason, status, delay_ms, retry_after,
  discard_message_id}`: a request failed for a passing reason and is re-sent
  after `delay_ms`. `reason`: `rate_limited` (429), `overloaded` (503/529),
  `server_error` (408, 425, 500, 502, 504, 520–528), `stream_error`,
  `disconnected` (stream ended before its finish marker, or the body failed),
  `stalled` (no bytes for 120 s, or no response started within 10 minutes; a
  response that keeps streaming has no overall limit) or `connect_failed`.
  `status` is the HTTP status or null; `retry_after: true` means the wait is
  the provider's `Retry-After`/`Retry-After-Ms`; `discard_message_id` names
  the partially streamed message of the failed attempt (null when nothing
  streamed). Local runtimes retry only on 429/503. At most
  `agent.model_retries` retries (default 3, max 10); waits double from
  `agent.retry_backoff_sec` with jitter (capped at 30 s); a `Retry-After`
  over 120 s is not waited for. Tools run only after a complete response, so
  a retry never repeats a tool. A request that is not retried fails the task
  with `Model provider returned HTTP <status>; <hint>: <message>` (hints for
  401/403, 402, 404, 429, 503/529) or `Provider reported an error while
  generating: <message>`; a remote provider's message comes from its JSON
  error body only, redacted and at most 300 bytes. A refused request adds no
  usage unless the provider reported tokens.
- `context.compacted {before_estimated_tokens, after_estimated_tokens,
  omitted_messages, response_token_limit, method, preserved, summary?,
  summary_model?, summary_ms?, fallback_reason?, pinned, rules_reapplied,
  requested?, focus?}`: after any compaction, the conversation's pinned
  answers (`/pin`, word for word, at most 8) and the folder guidance already
  delivered in the task are sent again in one system note (`pinned`,
  `rules_reapplied` count them); a later compaction replaces that note, and
  each later turn of a compacted conversation starts with the pins again.
  `/compact [what to keep]` (session meta `compact_request`) shortens the
  conversation at the start of the next turn whatever its size: every
  earlier step except the latest answer goes into the summary
  (`requested: true`, the focus goes to the summary). The request is used
  up either way; with nothing earlier to shorten, `agent.warning {kind:
  "compact"}` says so.
  `method`, `fallback_reason`: `method` is
  `model_summary` (`summary` at most 6 000 bytes) or `bounded_history`.
  `fallback_reason`: `disabled`, `context_too_small` (under 8 192 tokens),
  `offline_demo`, `timeout`, `empty_summary`, `summary_too_large`, or the
  request's error. The summary stays in the message tape.
- `model.request_timing {message_id, elapsed_seconds, first_text_seconds?,
  success, cancelled}` after each native foreground attempt (`success` is
  transport completion, not task acceptance).
- Prompt caching: OpenRouter requests to `anthropic/…` models mark the system
  prompt and a rolling point at the newest and previous request end with
  `cache_control`; `google/gemini…` gets one mark on the system prompt;
  `cached_tokens` is recorded when reported (`prompt_tokens_details.cached_tokens`,
  DeepSeek's `prompt_cache_hit_tokens`).
- Tool descriptions: models with 32K+ context, or hosted models with 16K+, get
  the complete native tool descriptions; smaller ones get them cut to 64 bytes.

### Spending limits (paid API models)

Limits on what paid per-token models cost: OpenRouter, or any
compatible endpoint that is not on this computer. Subscriptions (vendor CLIs),
models on this computer and the offline preview are never limited.

- Config `spending: {task_usd: number|null, daily_usd: number|null}`
  (defaults `1.0` and `10.0`; `null` turns a limit off; each between 0.01 and
  100000). The engine reads it again before every model turn, so a change in
  Settings applies to running tasks. A project's own config cannot change it.
- A task's paid requests are counted as they are priced (see `Usage` above),
  including failed attempts, compaction summaries and every subagent's
  requests, which count toward the task the user started. Costs worked out
  from the price list count and are marked `estimated`. The day's total spans
  all tasks and projects of the profile and starts again at local midnight
  (`native_meta` `spending_day`).
- Before each model turn (never inside a tool call) the task checks its
  limits:
  - Event `spend.notice {job_id, kind, spent, limit, estimated, text}` once
    per task (`kind: "task"`) or once per day (`kind: "daily"`) at 75%.
  - Event `spend.limit_reached {id, job_id, kind, limit, spent, estimated,
    raise_to, resets_at, title, text, continue_label}` at 100%: the task waits
    (status stays `running`) until the card is answered, the limit no longer
    applies, or the task is cancelled. A subagent at the limit shows the card
    in the task that started it (`job_id` is that task's job).
  - Event `spend.limit_resolved {prompt_id, job_id, kind, action, limit?,
    reason?, text?}`: `action` `continue` (the per-task limit, or today's
    limit, is raised to `raise_to`: the limit plus one more step of the
    setting, past what is already spent) or `stop`. `reason` is set when
    no one answered but the limit stopped applying (a setting changed, or
    the day's total reset).
  - Event `spend.unknown {job_id, model, text}` once per task when a paid
    request has no known price; it is not counted as $0.
- `POST /api/jobs/{id}/spending {prompt_id, action: "continue"|"stop"}`
  answers the waiting card of job `{id}` (the task's own job) → the
  `spend.limit_resolved` payload. `stop` cancels the task (and its
  subagents); its summary is "Stopped at your per-task spending limit…" (or
  daily). A wrong or answered `prompt_id` is 400.
- `POST /api/jobs` and `POST /api/commands/run` accept `max_cost_usd`
  (0.01–100000): this task's limit instead of `spending.task_usd` (the CLI's
  `--max-cost`).
- `GET /api/spending` → `{limits: {task_usd, daily_usd}, today: {day, usd,
  estimated, unknown_turns, limit, resets_at}, waiting: [card + {session_id,
  task_id}]}`; `today.limit` includes a raise for today.
- `GET /api/spending/estimate?session_id=&model=&draft_chars=` → `{show:
  false, reason: "not_paid"|"no_prices"}` or `{show: true, low_usd, high_usd,
  label, context_tokens, model, detail}`: the next message on a paid model
  with cached OpenRouter prices, from the conversation's saved message tape,
  the tool list, the draft length (`draft_chars / 3` tokens) and a typical
  answer: low = context × input price + 200 output tokens; high = three
  reads of the context (a few tool steps) + 4,000 output tokens. `model` is
  a picker id (default: the conversation's target). `label` is "about
  $0.01–$0.05" (or "less than $0.01").
- Automations: a run that reaches a limit stops with status
  `spending_limit` (or waits, when its approvals wait).

### Run record

`Job.run` (also `result.run` and the `agent.completed` payload's `run`) says
exactly what ran a job, recorded when it first calls its model:

```ts
RunRecord = {
  model_id: string,          // exact picker/registry id
  model: string,             // model name sent to the provider
  provider: string,
  route: "vendor_cli" | "local_llamacpp" | "native_http",
  vendor: string|null,       // "Codex", … when a vendor CLI ran it
  vendor_version: string|null, // the CLI's `--version` line
  effort: "low"|"medium"|"high"|null,  // null = the model's default
  app_version: string,
  app_commit: string|null,   // when the build recorded it
  settings_hash: string,     // first 12 hex of SHA-256 of the effective settings
  rules_hash: string|null,   // same, of the rules and skills text delivered
  recorded_at: number,
}
```

Command jobs have no run record. `rules_hash` hashes exactly what the agent
received (the vendor's `rules.delivered` text, or the rules part of the
native system prompt).

### App log

A log for bug reports: `<state>/logs/shadowcode.log`
(`~/.local/state/shadow-agent/logs/` by default), rotated at 5 MB into
`.1` and `.2`. Every line is `<local time> <LEVEL> <target>: <message>`,
passed through the secret redaction, with the home folder written as `~`.
It holds events, errors and timings only: engine warnings, finished jobs
(`job.finished job=… status=… model=… steps=… seconds=… tokens=…
cost_usd=…`, and the error for a failed one), and an allow-list of fields
per task event (for example `tool.completed tool success`, `model.retry
attempt max_attempts reason delay_ms`); never prompts, answers, tool
arguments or output, or file contents. `logging.level` (`error`, `warn`,
`info`, `debug`) sets how much the engine writes.

- `GET /api/logs` → `{folder, files: [{name, bytes}], max_file_bytes,
  max_files}` (newest first).
- `POST /api/logs/folder` → `{path}`: creates the folder. The desktop's
  `open_logs_folder` command calls it and opens that path; the window never
  supplies a path.
- Remote access refuses `/api/logs…`.

### Agent quality

- **Stuck.** When ShadowCode's own agent runs the same command and it fails
  the same way three times (same exit code and the same end of its output,
  numbers ignored), or changes a file back to an earlier version twice, the
  task records `agent.stuck {job_id, kind: "same_failure"|"edit_loop", text,
  detail, paused}` and pauses (`agent.stuck_check`, default on). The window
  offers Keep going (`resume`), Give a hint (`steer` then `resume`), Try
  another model (`cancel`, then the picker, keeping the task's mentions,
  "Only change these" and mode) and Stop; the card closes once the task
  runs again or ends. A task nobody can answer for there never pauses
  (`paused: false`): subagents, role steps, automation runs and jobs a
  connection owns (an ACP editor, the terminal UI, an MCP client). The agent
  gets a note to change course instead, and ACP shows the text as a
  thought. It fires once per loop; a command that later passes re-arms only
  its own loops.
- **Heads-ups.** A finished Code task (not a subagent) compares each file it
  changed with the file before the task: skip or focus markers added to
  tests, deleted test files, fewer tests or assertions, CI and hook files
  changed, lint and type checks switched off (`eslint-disable`,
  `@ts-ignore`, `# type: ignore`, `#[allow(…)]` …), loosened strictness and
  rewritten snapshots. Findings are `result.honesty = {count, text, flags:
  [{kind, path, line, text}]}` and the event `task.flags` (same payload).
- **Repaired tool calls.** Arguments that are almost JSON (a code fence,
  trailing commas, single quotes, raw newlines, JSON encoded twice) are
  repaired; a model served on this computer by another program (not the
  bundled `llamacpp` runtime, which parses calls through the model's
  template) that writes a call as text (`<tool_call>…</tool_call>`,
  `<|python_tag|>`, `<function=name>`, or an answer that is one JSON call)
  has it read as that call when the name is an offered tool. Only calls
  that end the answer count: one inside a code fence or followed by more
  text was only quoted and stays text. `<|python_tag|>` calls are separated
  by `;` outside argument strings. Each records `tool_call.repaired {from:
  "arguments"|"text", count}`.
- **Close edits.** `edit_file` with an `old_string` that matches nowhere is
  applied when it matches exactly one place ignoring line endings, spaces at
  line ends or indentation; the result carries `note`. Ignoring indentation
  still needs the same block structure (every line shifted by the same
  indentation), and the new text is re-indented by that shift; a new line
  indented less than the old text's lines is refused with an error that
  says so (not "old_string not found"). Two possible places refuse, as
  before. The approval card's diff and the new-package lookup use the same
  matching.

### Task timings

`Job.timings`, `Job.result.timings`, `agent.completed.payload.timings` and a
Compare lane's `timings` carry one optional snapshot (older records omit it):

```
Timings = {schema_version: 1, complete, total_seconds, queue_seconds, active_seconds,
           preparation_seconds, runtime_wait_seconds, model_load_seconds, model_reused,
           model_requests, model_requests_seconds, first_text_seconds, first_text_request,
           tool_batches_seconds, final_checks_seconds, check_process_seconds}   // unobserved: null
```

- `complete`: the task reached its terminal boundary (including failure or
  cancellation); it does not mean every category was measured or that the
  task passed verification. Durations are seconds. While a job is live,
  lookups observe its monotonic clock; snapshots saved at admission and in
  foreground responses are partial. Final timings are stored with the
  terminal job and event. A restart never estimates missing durations or
  rebuilds a monotonic clock from wall-clock times.
- `total_seconds`: local acceptance to the finish decision (or now),
  including queue time but not the final database commit. `queue_seconds`:
  acceptance to engine admission, including workspace/local admission and
  worker waiting (equals total for a task cancelled before admission).
  `active_seconds`: admission to the finish (null if never admitted);
  includes preparation, approvals, pauses, tools, provider work and cleanup.
- `preparation_seconds`: managed local preparation, including catalog checks
  and runtime acquisition; `runtime_wait_seconds` and `model_load_seconds` are
  subsets. Load includes stopping a previous runtime and starting and
  readiness checks for the new one; it is not GPU weight-transfer time.
  `model_reused`: true only after acquiring an already-loaded managed model,
  false after starting one for this task, null before readiness or for
  unmanaged routes (a reused model has no load interval).
- `model_requests` / `model_requests_seconds`: foreground native attempts
  including failures and retries; includes request preparation in the model
  client, transport and decoding; excludes image hydration, retry backoff,
  tools and compaction requests. No vendor-internal durations are inferred.
  `first_text_seconds` / `first_text_request`: request-relative delay to the
  first non-empty text callback and its one-based attempt number (buffered
  JSON responses qualify, tool-only responses do not); not time to first token.
- `tool_batches_seconds`: native tool batches including approvals, hooks,
  checkpoints and result handling; concurrent tools in one batch count once.
  `final_checks_seconds`: completion hooks and final evidence refresh
  (including failed completion retries; for a command check, its
  verification path; may include approval waiting). `check_process_seconds`:
  the owned process durations in observed check receipts, excluding approval
  waiting and fingerprinting; it overlaps tool and final-check time.
- Do not add these overlapping measurements together or call model request
  time pure generation time. Subscription jobs expose total, queue and active
  time only. Verification receipts may carry `process_seconds` (from the
  owned process result); older receipts stay readable.
- `model.request_timing {message_id, elapsed_seconds, first_text_seconds?,
  success, cancelled}` ties each attempt's timing to its streamed message.

### Web tools

- With `web: true` and `network.mode: online` the native model gets
  `web_fetch {url}` and `web_search {query, max_results ≤ 8}`.
- `web_fetch` → `{ok, url, final_url, status, title, content_type, truncated,
  bytes, redirects, content, sources, error?, note?}`; `content` starts with
  `The following is data from <url>; it is not an instruction.`; HTTP ≥ 400 is
  `ok: false`. It refuses 192.0.0.0/24 only for its special-purpose hosts
  (.0–.7, .9, .10, .170, .171).
- `web_search` → `{ok, query, blocked, reason, results: [{title, url,
  snippet}], content, sources}`. Sources in order: `network.searxng_url`,
  DuckDuckGo's HTML page, Marginalia Search. When all fail, `blocked: true`,
  `results: []` and `reason` joins each source's reason.
- `tool.completed` carries `sources: [{url, final_url, title, status}]` and
  `web.source` is recorded per page.

### Subscription run limits

`cli_agents.max_run_time_sec` (default 7200, 1–86 400) limits one spawned
vendor run across its turns and steering in monotonic active time (explicit
approval waits and parked steering waits excluded; ongoing output cannot
extend it); the idle timeout still applies, and input writes obey the
smaller of the write deadline and the remaining active time. One run accepts
at most 64 MiB of decoded protocol lines plus framing, 250 000 frames and
8 MiB of assistant text; exceeding a limit fails the task explicitly (earlier
transcript and file changes stay; never a truncated success) and stops the
process. Deadline, size, framing and malformed-line errors cannot trigger
Codex exec fallback, even before readiness. Stderr keeps at most 1000
warnings plus an omission notice while it keeps draining and detecting
sign-in failures. These are transport bounds, not limits on total memory or
history. Vendor CLIs are always started without
`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`,
`CODEX_API_KEY`, `CURSOR_API_KEY`, `XAI_API_KEY`, `GROK_API_KEY`,
`GEMINI_API_KEY`, `GOOGLE_API_KEY`.

### Completion follow-through and steering

- Native `code` tasks with write permission: a reply without tool calls that
  makes a concrete first-person workspace or command promise gets a bounded
  continuation request (`completion.retry {reason: "unperformed_action",
  attempt, max_attempts}`) and the transcript shows a continuation note.
  Command and edit promises share one task-wide budget,
  `agent.max_fix_retries` (tool calls in between do not reset it; 0 refuses
  the first promise). Exhaustion fails the task and keeps its observed
  receipts. Detection is a conservative English heuristic, not a
  completeness or truthfulness oracle: quoted or fenced examples,
  conditional offers, deferrals, negative commands and explanations are
  excluded where recognized; plan, review and read-only tasks are outside
  it. A continuation runs no tool itself; the usual task, permission,
  approval, step and token limits apply, and unsupported claims of success
  still need verification evidence.
- Steering is checked before accepting a final answer, after completion
  hooks, and before each remaining tool batch; a quick pause/steer/resume
  keeps the instruction even if the worker never parked. A newer instruction
  supersedes pending tool proposals: their tool messages get `success:
  false`, `output.execution_status: "not_run"`, `output.reason:
  "superseded_by_steering"` (inserted before the steering note) and produce
  no receipt or `tool.started`. Completed calls stay recorded and are not
  replayed. Failure evidence from completion hooks comes before a newer
  steering note; a later completion candidate still runs the configured
  completion checks.

## Review and rewind

- `GET /api/review/tasks/{task_id}` → `{task_id, session_id, workspace, busy,
  files: [{path, status: "added"|"modified"|"deleted"|"unchanged"|"unavailable",
  source: "checkpoint"|"git"|"none", added, removed, binary, error?}]}`: only
  the files this task changed, compared with the checkpoint taken before its
  first write (`checkpoint`) or, for paths a vendor CLI reported, with the
  last commit (`git`). `busy`: a task is queued or running in the project.
- `GET /api/review/tasks/{task_id}/file?path=` → the row plus `hash` and
  `hunks: [{id, header, old_start, old_len, new_start, new_len, lines:
  [{kind: "add"|"del"|"ctx", text, eol?: false}]}]`. A secret file (`.env`,
  keys, credential files) answers `secret: true` and no hunks; it can still
  be undone as a whole.
- `POST /api/review/tasks/{task_id}/explain {path}` → `{ok: true, path, text,
  model, source: "model"|"local"}` or `{ok: false, path, error}`: the file's
  change (at most about 24 KB of diff) explained in plain words by the
  conversation's model, or the model loaded on this computer when the
  conversation uses a subscription. Only on request; secret and binary files
  are refused.
- `POST /api/review/tasks/{task_id}/undo {path, hunk?}` → the file's review
  after putting one hunk (by `id`) or the whole file back as it was before the
  task. Refused while a task runs in the project, outside the open project,
  and when the file changed since the hunk was computed. Adds a process note
  to the message tape and a `review.undone` event.
- `GET /api/checkpoints/tasks/{task_id}` → `{rewindable, checkpoint: {task_id,
  workspace, changes, paths, restored, rewind_paths, kept, unreported}}`:
  `rewind_paths` is what a rewind restores now, `kept: [{path, reason:
  "saved_by_you"|"edited_by_you_and_agent"}]` the files it leaves alone, and
  `unreported` the files that changed during a subscription turn although the
  agent did not report editing them.
- `POST /api/checkpoints/tasks/{task_id}/restore` → `{ok, restored: string[],
  undo_id: string|null}`. The task's project must be the selected one, trusted,
  writable, and idle. Before writing, the files are recorded as they are
  (checkpoint rows of task `rewind:<undo_id>`, `native_meta`
  `rewind_undo:<undo_id>`). Writes `checkpoint.restored`. Restores files
  changed by file tools, shell commands and subscription turns. Files you
  saved in the editor during a subscription turn are kept; body
  `{include_user_edits: true}` also rewinds the ones the agent edited too.
- `POST /api/checkpoints/rewinds/{undo_id}/undo` → `{ok, restored, task_id}`:
  puts them back once (refused if they changed since); the task can then be
  rewound again. Writes `checkpoint.rewind_undone`.

## Second opinions

A read-only review of the staged changes or of one task's changes by a
model the user picks, or another model's view of a task's answer. Each runs
as an ordinary job in `review` mode (read-only: no write, shell or MCP tools
natively; vendor CLIs in plan/read-only mode with every edit or command
request denied) in a hidden conversation (`session_meta` `second_opinion`,
and `second_opinion_of` = the conversation it belongs to), queued behind any
task in the project. Hidden conversations never appear in `GET
/api/sessions` and are deleted with the conversation they belong to (unless
still running). Records are `native_meta` `second_opinion:<id>`, indexed per
project (`second_opinion_index:<project>`, newest first, at most 40; the
oldest finished ones and their conversations are removed). Nothing is
written when a request is refused.

- `POST /api/second-opinions {kind: "review"|"ask", source: "staged"|"task",
  workspace?, session_id?, task_id?, model, question?, consent?}` → the
  record, or `{ok: false, status: 409, error, needs_consent: true, handoff:
  {from, to, excerpt_chars, images: 0, reason, purpose: "second_opinion",
  files}}`. `model` is a picker id. `staged` reviews `git diff --cached` of
  the project (an error when nothing is staged); `task` reviews the task's
  changes as `GET /api/review/tasks/{id}` shows them, and `ask` adds the
  task's request and answer. The reviewer gets at most 60 000 bytes of diff
  (whole lines; later files are only named, `truncated: true`), never the
  contents of secret-looking files (`omitted`), and an optional `question`
  (at most 2 000 characters). Offline mode refuses models that do not run
  on this computer. A cloud reviewer needs `consent: true` when the
  conversation's last turn ran on this computer or the model that wrote the
  change did. `session_id` (staged reviews) is the conversation the user is
  in; without it, the conversation of the latest turn in the project that
  changed files.
- `GET /api/second-opinions?workspace=&session_id=&task_id=&source=&limit=20&diff=`
  → `{workspace, second_opinions: Record[]}` newest first; running records
  are brought up to date with their jobs. Records come without their
  reviewed `diff` (`[]`) unless `diff=1`.
- `GET /api/second-opinions/{id}` → the record. `POST …/{id}/cancel` stops a
  running one.
- `POST /api/second-opinions/{id}/findings/{finding} {status:
  "open"|"dismissed"}` → the record.
- `POST /api/second-opinions/{id}/findings/{finding}/fix {consent?}` →
  `{second_opinion, job}`: queues a `code` task in the conversation the
  record belongs to (a new conversation when it no longer exists) on that
  conversation's model, with the finding as its request; the finding becomes
  `fixing` with `fix_job_id` and `fix_session_id`. A second fix of the same
  finding is refused. The job start follows `POST /api/jobs` (trust, 409
  consent).
- `GET /api/second-opinions/options?workspace=&session_id=&task_id=` →
  `{workspace, prefs: {model: string|null, before_commit: bool}, offline,
  writer: {model, label, local}|null, local_only}`: what the window needs to
  suggest a reviewer (not the writer; only local models for `local_only`).
- `GET /api/second-opinions/current?workspace=&source=staged|task&task_id=`
  → `{hash, files, omitted, truncated}`: the fingerprint of the changes as
  they are now; a record whose `diff_hash` differs reviewed other changes.
- `POST /api/second-opinions/prefs {workspace?, model?, before_commit?}` →
  prefs (`native_meta` `second_opinion_prefs:<project>`). Starting a second
  opinion also remembers its model. "Review before every commit" is a window
  behaviour: the engine never refuses a commit because of it.

Record: `{id, kind, workspace, source, session_id, task_id, question,
reviewer: {model, label, local}, writer: {model, label, local}|null,
same_model, consented, job_id, review_session, review_task, status:
"queued"|"running"|"completed"|"failed"|"cancelled"|"limit_reached"|"interrupted",
created_at, finished_at, diff_hash, files, omitted, diff: [{path, status,
diff, binary}], truncated, context_chars, summary, findings: Finding[],
format_note, error, usage: Usage, model_name, reviewer_changed, redacted}`
(`redacted`: credentials recognised in the request and replaced before it
was sent).
`Finding`: `{id: "f1"…, file, line, end_line, hunk (header of the reviewed
hunk), severity: "high"|"medium"|"low"|"info", title, explanation,
suggested_fix, status: "open"|"dismissed"|"fixing", fix_job_id,
fix_session_id}`. Reviewers are asked for one JSON object; fenced, prose-
wrapped, reasoning-prefixed, differently keyed, trailing-comma, cut-off and
Markdown-list replies are read too (at most 50 findings), and a reply that
cannot be read stays as `summary` with a plain `format_note`. For `ask`,
`summary` is the whole reply. `reviewer_changed` lists files the reviewer's
job reported changing (it should be empty).

Job summaries (`GET /api/jobs?view=summary`, the feed) carry
`second_opinion` (the record id, null for other jobs); the window leaves
these out of the queued follow-ups. The engine broadcasts
`second_opinion.updated {id, status, workspace, kind}` (with the record's
`session_id`) when a second opinion starts, finishes, stops or a finding
changes; it is a wake-up only, never stored.

## Slash commands and memory

- `GET /api/commands` → `{commands: [{name, description, arg_spec, alias,
  source, kind}], issues: string[]}`: built-ins plus project workflows and
  skills (including `.claude/commands/*.md`; `arg_spec` is the command's
  `argument-hint`). A project skill named like a built-in is listed as
  `skill <name>`; other clashes and ambiguous names are reported in `issues`.
- `POST /api/commands/run {name, args?, session_id?, model?, purpose?, queue?}` →
  `CommandResult = {handled, text, kind: "text"|"card"|"list"|"diff"|"approval"|"error"|"overlay"|"quit",
  icon, headline, body, items: [{label, value}], diff, path, approval_action,
  approval_reason, overlay, quit, passthrough, metadata}`. `args` at most 64 KB.
  Workflow commands start a job and return it in `metadata.job`; others may
  set `metadata.panel`, `metadata.action`, `metadata.session_id` or
  `metadata.reload_config`. Cards that are not navigation are stored as
  `command.completed` (redacted). Remote access refuses `/run`, `/test
  <command>` and `/background` unless terminals are allowed, and `/diff` or
  `/why` of a secret file.
- `POST /api/memory {action?: "read"|"append"|"replace", scope?: "project"|"task",
  session_id?, task_id?, note?, expected_hash?}` → `{ok, project, task,
  project_hash, task_hash, task_id, text, inherited_notes?, inherited_truncated?}`.
  Project notes live in `.shadow/memory/project.md`; task notes in the
  profile. `replace` needs the `expected_hash` from a read. Writes need a
  trusted, writable project. Inherited notes (from a branch) are shown up to
  4000 characters.

## Goals

```
Goal = {id, workspace, instruction, title?, status, progress, progress_pct, running,
        milestones: [{id, title, status: "pending"|"in_progress"|"done"|"failed", detail?, task_id?,
                      require_verification, mode: "code"|"plan"|"review"}],
        updated_at, session_id?, job_id?, run_detail?}
```

- `GET /api/goals?all=` → `{goals}` for the selected project (all with
  `all=true|1`), newest first, at most 500.
- `POST /api/goals {instruction, workspace?, session_id?, run?, milestones?}`
  → `Goal`. `instruction` 1–100 000 bytes; `milestones` (strict, 1–32 of
  `{title, mode?, require_verification?}`) default to a built-in plan;
  `require_verification` only with `mode: "code"`. `session_id` must belong
  to the project. `run: true` starts it and selects its conversation.
- `GET /api/goals/{id}` → `Goal`; `DELETE /api/goals/{id}` → `{ok: true}`.
- `POST /api/goals/{id}/run {session_id?}` → `Goal` (refused while running or
  when every milestone is done).
- `POST /api/goals/{id}/pause`, `POST /api/goals/{id}/abandon` → `Goal`.
- `POST /api/goals/{id}/milestones/{milestone_id} {status, detail?}` → `Goal`;
  refused while the goal runs. `detail` at most 16 000 bytes.

## Automations

Scheduled prompts ([AUTOMATIONS.md](AUTOMATIONS.md)). Stored in SQLite
(`automations`, `automation_runs`); conversations a run creates carry
`session_meta` `automation_id`, `automation_run` and, while in a temporary
worktree, `automation_worktree`.

```
Automation = {id, workspace, name, prompt, model /* "" = the project's */, mode: "code"|"plan"|"ask",
              schedule: Schedule, timezone: "local"|"utc", options: Options, paused,
              next_run_at: number|null, created_at, updated_at,
              description, running_run: string|null, last_run: Run|null}
Schedule = {kind: "hourly", minute} | {kind: "daily", time: "HH:MM"} | {kind: "weekdays", time}
         | {kind: "weekly", day: 0-6 /* Sunday first */, time} | {kind: "cron", expr}
Options = {checkout: "worktree"|"main", permission: "project"|"read_only", on_approval: "stop"|"wait",
           max_runtime_minutes /* 1–1440, 60 */, catch_up_minutes /* 0–10080, 120 */, notify /* true */}
Run = {id, automation_id, status, trigger: "schedule"|"catch_up"|"manual", scheduled_for: number|null,
       started_at, finished_at: number|null, duration: number|null, session_id, job_id, task_id,
       summary, detail, usage: Usage|null, worktree: {id, path, branch, base_commit, removed?}|null, missed}
```

- `cron` takes five fields or `@hourly`, `@daily`, `@weekly`, `@monthly`,
  `@yearly`; when day-of-month and weekday are both restricted either matches.
- Run `status`: `running`, `completed`, `failed`, `cancelled`, `timed_out`,
  `needs_approval`, `interrupted`, `missed`, `skipped`. The newest 200 runs
  per automation are kept.
- The draft (create/update body `{name, prompt, model, mode, schedule,
  timezone, options}`) is typed: a mistyped field fails with "Automation
  settings are not valid"; unknown fields are ignored. `name` 1–80
  characters without control characters; `prompt` 1–32 000 bytes; `model` at
  most 512 bytes.

Routes:

- `GET /api/automations?all=` → `{workspace, automations, scheduler, now}`;
  `scheduler` is false in engines that do not run schedules (one-shot CLI).
- `POST /api/automations {…draft, workspace?}` → `Automation`. Needs a
  trusted project; refused when the schedule never runs; at most 50 per project.
- `POST /api/automations/preview {schedule, timezone}` → `{ok: true,
  description, next: [3 times], now}` or `{ok: false, error}`.
- `GET /api/automations/{id}?limit=` → `Automation` + `runs` (newest first,
  default 50, max 200); `GET /api/automations/{id}/runs?limit=` → `{runs}`.
- `POST /api/automations/{id}` (draft) → `Automation`; the next time is
  recomputed from now (none while paused).
- `DELETE /api/automations/{id}` → `{ok: true}`; refused while it runs. Its
  conversations stay.
- `POST /api/automations/{id}/pause` → `Automation` (`next_run_at: null`);
  `…/resume` → the next time after now (paused time is not caught up).
- `POST /api/automations/{id}/run` → the new `Run` (`status: "running"`);
  refused while one runs. `…/stop` cancels the run's job and returns its
  finished `Run`.

Scheduler: the desktop and `shadowcode serve` tick every 20 s. A due time
runs when at most 2 minutes late, or later within `catch_up_minutes`
(`trigger: "catch_up"`); older ones write one `missed` row with `missed` =
the number of times. A time that comes while a run is going writes a
`skipped` row. Runs use the automation's model, else the project's execution
target, else the configured model; `ask` runs as review. Pending approvals
for the run's task stop it (`needs_approval`) unless `on_approval` is `wait`
(`automation.waiting` is recorded). Remote clients may manage automations in
trusted projects; runs never auto-approve.

## Compare

One task on 2–3 models, each in its own managed worktree
([COMPARE.md](COMPARE.md)). `workspace` defaults to the selected project.

```
Compare = {id, workspace, task, mode, web, created_at, finished_at: number|null,
           state: "running"|"done"|"needs_review"|"applied"|"discarded",
           base: {commit, head, included_uncommitted},
           lanes: [{model, name, session_id, job_id, worktree, worktree_id, branch, base_commit,
                    status, summary, changed_files: [{path, status, additions, deletions, binary}],
                    changed_files_truncated, checks: {passed, failed, incomplete?, commands: [{command, exit_code, success, state?}]},
                    duration_s, timings, local_progress: {model_id, phase}|null, local_runtime, usage, error, removed}],
           winner: string|null, applied_files: string[], cleanup_pending, recovery: object|null, notes: string[]}
```

- `POST /api/compare {task, models: [2–3 distinct picker ids], workspace?, mode?:
  "code"|"plan"|"ask", web?}` → `Compare`. The project must be a trusted Git
  repository root with a first commit and no unresolved merge conflicts;
  every lane starts from HEAD plus uncommitted, non-ignored work (a "ShadowCode
  compare base" commit reachable only from lane branches); the checkout and
  index are never touched. Offline mode accepts only models that run on this
  computer; managed local lanes run one after another. Refused while
  app-owned editor recovery drafts differ from their saved base (the refusal
  names paths only).
- `GET /api/compare/{id}` → `Compare`, refreshed with the lanes' status.
- `GET /api/compares?workspace=` → `{compares: Compare[]}`, newest first (up to 20).
- `POST /api/compare/{id}/keep {model, accept_unverified?}` → `Compare`
  (`state: "applied"`): commits the lane's result on its branch, checks it
  with `git apply --check` and applies it to the working tree only (never
  staged or committed); stops a running lane first; refuses and names the
  conflicting files when it no longer applies; removes every lane worktree
  and `shadowcode/…` branch.
- `POST /api/compare/{id}/recover` → `Compare`: finishes or reviews an
  interrupted Keep (`state: "needs_review"`); Discard is refused until then.
- `POST /api/compare/{id}/discard` → `Compare` (`state: "discarded"`): stops
  lanes and removes their worktrees and branches.
- `POST /api/compare/{id}/cancel` → `Compare`: stops lanes and keeps their
  worktrees; `done` once they stop.
- `GET /api/compare/scoreboard?workspace=` → `{workspace, rows: [{model, name,
  wins, runs}]}` (a comparison counts once all lanes finish or a result is kept).

Lane conversations are hidden from `GET /api/sessions` unless
`include_compare=true`; their rows and `GET /api/sessions/{id}` carry
`compare_id` and `compare_lane`. Lane worktrees count toward the 64 managed
worktrees. App-owned workspace mutations (file saves, instructions, skills,
attachments, editor drafts, project-map saves, Git hunk/stage/commit)
serialize with Compare's admission through a project mutex and, in Git
repositories, the repository advisory lock.

## Worktrees

### Managed worktrees

```
ManagedWorktree = {id, source, path, branch, base_commit, common_directory, state, created_at, detail}
```

Every `POST /api/worktrees…` that sends a non-empty `workspace` is refused
when it is not the selected project ("Project changed; refresh worktrees
before continuing"). Reviews need a trusted project; changes need a trusted,
writable, idle project. Changes are two-step: a review answers a `hash`, and
the change must send that `hash`.

- `GET /api/worktrees` → `{workspace, worktrees: ManagedWorktree[]}`.
- `POST /api/worktrees {workspace, reference?}` → `ManagedWorktree` (from
  `reference`, default `HEAD`); records `worktree.created`.
- `POST /api/worktrees/inspect {workspace, id}` → `{record, head,
  current_branch, status, can_remove, reason, hash}`.
- `POST /api/worktrees/remove {workspace, id, hash}` → `ManagedWorktree`; `worktree.removed`.
- `POST /api/worktrees/review-changes {workspace}` → `{source, head,
  staged_diff, unstaged_diff, untracked: [{path, bytes, hash, mode}],
  intent_to_add, hash}`; `POST /api/worktrees/copy-changes {workspace, hash}`
  → `ManagedWorktree` (copies the project's uncommitted work into a new
  worktree); `worktree.changes_copied`.
- `POST /api/worktrees/review-return {workspace, id}` → `{record, source_head,
  source_branch, worktree_head, worktree_branch, merge_base, diff, hash}`;
  `POST /api/worktrees/return {workspace, id, hash}` → `ManagedWorktree`; `worktree.returned`.
- `POST /api/worktrees/review-repair {workspace, id}` → `{record,
  administrative_directory, head, checkout_pointer, registration_pointer,
  warning, hash}`; `POST /api/worktrees/repair {workspace, id, hash}` →
  `ManagedWorktree`; `worktree.repaired`.
- `POST /api/worktrees/recovery {workspace, id}` → `{record, commit, branch,
  warning, hash}`; `POST /api/worktrees/restore {workspace, id, hash}` →
  `ManagedWorktree`; `worktree.restored`.

### Worktree tasks

A task can start as a new conversation in a fresh managed worktree, so it
runs while another task runs in the main checkout.

```
WorktreeTask = {id, workspace /* the project */, session_id, worktree, branch,
                base: {commit, head, included_uncommitted}, task, created_at, finished_at: number|null,
                state: "starting"|"running"|"done"|"applied"|"branch"|"discarded", job_id,
                status /* the conversation's latest job status */, changed_files, changed_files_truncated,
                applied_files: string[], conflicts: string[], conflict_detail, kept_branch: string|null,
                notes: string[], removed, port: number|null,
                setup: {} | {ok, copied: string[], skipped: [{path, reason}], port,
                        commands: [{command, ok, exit_code, seconds, output}]}}
WorktreeSetup = {copy: string[] /* ≤ 20 project files */, setup: string[], teardown: string[]
                 /* ≤ 10 one-line commands each */, port_start /* ≥ 1024 */, port_end}
```

- Start: `POST /api/run` (or `/api/jobs`) with `worktree: true` and the usual
  `workspace`, `task`, `model`, `images`, `web`, `handoff_consent`. The engine
  captures HEAD plus uncommitted, non-ignored files as a base commit (the
  project's index and files are untouched), creates a worktree on branch
  `shadowcode/<id>`, trusts it, copies the composer's `.shadow/attachments/…`
  files named in the task, creates the conversation there and starts the job
  with the project's permission level as the ceiling. The answer is the job
  plus `worktree_task`. If the job cannot start (including consent)
  everything is removed. Refused outside a Git repository root, with
  unresolved conflicts or no first commit, or for a local GGUF model while
  another task runs on a different local model.
- `base_branch` (optional): start from that local branch's last commit
  instead of the current files (`base.included_uncommitted: false`);
  refused when it is not a local branch.
- Setup: each new worktree gets the project's `WorktreeSetup`. Its `copy`
  files (regular files up to 10 MB, never outside the project or in `.git`)
  are copied from the project, then its `setup` commands run in the worktree
  with `sh -c` as the user (`CI=1`, 10 minutes each), stopping at the first
  failure; the task starts either way and `setup` records what happened. The
  task gets a port that is free on this computer and not used by another
  open worktree task of the project; its shells, setup commands and
  subscription CLIs see it as `PORT` and `SHADOWCODE_PORT` (`session_meta`
  `task_env`). Before the worktree is removed (apply, keep, discard) its
  `teardown` commands run (2 minutes each); failures are added to `notes`.
- `GET /api/worktree-tasks/setup?workspace=` → `{workspace, setup:
  WorktreeSetup, suggested: WorktreeSetup}`: `suggested` comes from the
  project's lockfiles (`npm ci`, `pnpm install --frozen-lockfile`,
  `yarn install --frozen-lockfile`, `bun install --frozen-lockfile`,
  `uv sync`, `poetry install`) and `.env`, `.env.local`,
  `.env.development`; nothing runs until the user saves it.
- `POST /api/worktree-tasks/setup {workspace?, setup: WorktreeSetup}` →
  `{workspace, setup}`. Storage: `native_meta` `worktree_setup:<project>`.
- `GET /api/worktree-tasks?workspace=` → `{workspace, tasks}` (newest first,
  at most 30); `GET /api/worktree-tasks/{id}` refreshes one.
- `POST /api/worktree-tasks/{id}/apply` → `WorktreeTask`: needs no turn
  running in it and none in the main checkout; commits the result on the
  worktree's branch, then `git apply --check` of `base..result`; on refusal
  nothing is written, `state` stays `done` and `conflicts`/`conflict_detail`
  explain. Otherwise the result is applied to the working tree only and
  `state: "applied"`.
- `POST /api/worktree-tasks/{id}/keep-branch` → `WorktreeTask` (`state:
  "branch"`, `kept_branch`).
- `POST /api/worktree-tasks/{id}/discard` → `WorktreeTask`: stops a running
  turn (waits up to 60 s), `state: "discarded"`; repeating retries cleanup.
- Closing (apply, keep, discard) removes the worktree (and, except for keep,
  its branch), stops trusting it, moves the conversation back to the project
  (vendor session ids are forgotten), follows the selection when it was open,
  and records `worktree_task.closed`. Session rows carry `worktree_task` and
  `worktree_source` while the worktree exists; `DELETE /api/sessions/{id}` is
  refused until then. Storage: `native_meta` `worktree_task:<id>` and
  `worktree_task_index:<project>`; `session_meta` `worktree_task`, `worktree_source`.

### Parallel checkouts

Prepared worktrees for splitting a goal into up to 4 explicit tasks
([NATIVE_PARALLEL.md](NATIVE_PARALLEL.md)); nothing is dispatched automatically.
Actions need a trusted, writable project.

- `GET /api/parallel` → `{max_workers: 4, plan: ParallelPlan|null, note}`;
  `ParallelPlan = {id, goal, source, lead_note, verify_status, workers: [{item:
  {id, title, prompt}, worktree_path, branch, status}]}`.
- `POST /api/parallel/prepare {goal}` → `{ok: true, enabled, max_workers,
  plan, note}` or `{ok: false, enabled: false, error, max_workers}` outside a
  Git repository. One plan per project.
- `POST /api/parallel/worker-status {worker_id, status: "ready"|"running"|"finished"|"failed"}`
  → `{ok, worker, status}`.
- `POST /api/parallel/verify` → `{ok, verify_status, checked, conflicts:
  [{worker, branch, …}]}` or `{ok: false, verify_status: "waiting", unfinished}`.
- `POST /api/parallel/cleanup` → `{ok, cleaned}`.

## Subagents

See [SUBAGENTS.md](SUBAGENTS.md).

- `GET /api/agents?workspace=` → `{agents, shadowed, issues, dirs, settings,
  user_dir, workspace}`. `agents[]`: `{name, description, model, tools, deny,
  mode: "read-only"|"write", max_turns, source: "builtin"|"project"|"user",
  path, hash, ignored, instructions_preview}`; `settings` is the effective
  `subagents` config.
- `GET /api/subagents?session_id=` (required) → `{runs}`, oldest first.
- `GET /api/subagents/{id}` → `{id, agent, description, prompt, mode, model,
  parent_session, parent_task, parent_job, job_id, session_id, status,
  summary, error, files: [{path, status, additions, deletions, binary}],
  files_truncated, binary_files, patch, applied, usage, steps, depth, notes,
  created_at, finished_at, role, model_id, runner, vendor, route, cost,
  verdict}`. Since 1.0: `role` is `""` or `plan|implement|review|explore`,
  `runner` is `shadowcode|vendor`, `route` is `local|cloud`, `cost` is
  `local|subscription|api`, `verdict` is `ready|needs_changes|null`; `usage`
  gains `cost_estimated` and `source`.
- Native tools: `spawn_agent {agent?, prompt, description?, model?, write?}`
  or `{tasks: [...]}` (at most 8); `apply_agent_changes {run_id}` (runs as
  `apply_patch`); `load_skill {name}`; approved MCP tools as
  `mcp__<server>__<tool>`. A subagent's approvals carry the parent's
  `session_id` and a reason starting `Subagent <name>:` (a role's:
  `<Role> role (<model>):`), including a vendor CLI subagent's permission
  requests. `spawn_agent` results add `model` and, for a role, `role`.

### Roles

- `GET /api/roles?workspace=&session_id=&model=` → `{workspace, setup,
  roles, presets, conversation: {id, name, local}, offline, consented}`.
  `setup` = `{pipeline, plan, implement, review, explore, preset,
  updated_at}`; each value is `""` (the conversation's model), `"skip"` (plan
  and review) or a picker id. `roles.<plan|implement|review|explore>` =
  `{role, label, setting, id, name, provider, local, runner, vendor, cost,
  skipped, blocked?, needs_consent?}`; `blocked` explains why the role cannot
  run (offline, vendor turned off, model unavailable) and `needs_consent`
  marks a cloud role a conversation on this computer has not allowed.
  `presets[]` = `{id, label, description, roles}`; `consented` lists the
  providers the conversation allowed.
- `POST /api/roles {workspace?, session_id?, model?, preset?, pipeline?,
  plan?, implement?, review?, explore?}` → the same view. Absent fields keep
  their value; `preset` is applied first; changing a role by hand clears
  `preset`. Refused: unknown presets or models, skipping implement or
  explore, a preset needing a local model when none is ready, untrusted
  projects. Stored per project in ShadowCode's database (`roles:<project>`),
  never in the repository.
- `POST /api/jobs` with `roles: true`: the job's `model` names the roles and
  `routing` is `{purpose: "roles", provider: "shadowcode:roles", model_id:
  "roles:<role>=<id>,…", model_name, inference: local|cloud, route:
  "roles"}`. When a cloud role would receive a local conversation's work,
  the answer is `needs_consent` with `handoff: {from, to, excerpt_chars,
  images: 0, reason, roles: [{role, label, name, provider, agent?}]}`;
  resending with `handoff_consent: true` records the providers in the
  conversation (`session_meta` `consent:cloud_roles`). A plain turn that
  starts with an `@agent` whose role or definition model is a cloud one asks
  the same way. Offline, a cloud role is refused with the reason.
- Task events: `roles.started {label, stages}`, `plan.updated` (one step per
  role plus "Apply the changes"), the roles' `subagent.*` events,
  `tool.started/completed` for `apply_agent_changes`, and `roles.finished
  {label, stages[{role, label, name, model_id, runner, vendor, route, cost,
  status, skipped, error, run_id, session_id, usage, files, additions,
  deletions, verdict, duration_s}], applied: bool|null, apply_note, files,
  completed}`.

## Settings and health

### Configuration

- `GET /api/config` → the effective configuration for the selected project
  plus derived, read-only fields:
  `permissions.vendor_notes: {native, codex, claude, cursor, grok, antigravity, network}`
  and `network.offline`. Sections: `model`, `permissions`, `agent`, `ui`,
  `onboarding`, `routing`, `mcp`, `hooks`, `verification`,
  `trusted_workspaces`, `guardian`, `cli_agents`, `local_engine`, `network`,
  `sandbox`, `checkpoints`, `limits`, `updates`, and, when saved, `code_intel`
  and `voice`. Every key and its default is documented in
  `config.example.yaml`, which `native/core/tests/config_keys.rs` keeps
  complete. Keys this version does not read are kept and returned as they are
  (for example the retired `git` and `logging` groups, `ui.host`, `ui.port`,
  `ui.ability` and `permissions.profile` from older configs, or keys from a
  newer version); they have no effect.
- Errors name the key and the fix: a value of the wrong type is
  "`<path>/config.yaml: `agent.max_steps` has a value ShadowCode can't read
  (…). Fix the value, or delete that line to use the default.", and a value
  out of range is "`<path>/config.yaml needs a fix: agent.max_steps must be
  between 1 and 1000; found 0`". `PUT /api/config` reports the same messages
  without the file name.
- `PUT /api/config {values, api_key?, api_key_env?}` → the configuration.
  `values` (an object, required) is merged into the saved file and the
  result validated; derived fields are ignored. `api_key` is never written to
  the file: it goes to `secrets.env` under `api_key_env` (default the model's
  `api_key_env`). A `values.model` change gets a stable model id and must not
  reuse another target's id. Setting `permissions.mode` also sets
  `approve_shell: true` unless the same request sets it.

Settings with API-visible meaning:

- `permissions.mode: "ask"|"allow_edits"` (default `allow_edits`): `ask` —
  shell and file edits ask; `allow_edits` — edits inside the project are
  allowed, shell asks. Configs without a mode migrate on load: `workspace` →
  `allow_edits`; `read_only` stays; `elevated` → `allow_edits` with
  `approve_shell: true`, unless both `approve_shell: false` and
  `require_approval_for_dangerous: false` were set. `permissions.level`
  (`read_only|workspace|elevated`), `network`, `allow_root` and
  `approve_shell` remain as advanced fields. Edits outside the project
  (including through symlinks) fail before any approval. Privileged shell
  commands (sudo, su, pkexec, doas, run0) are denied, or asked with
  `allow_root`; destructive Git commands always ask; network commands are
  denied offline.
- `agent.model_retries` (0–10, 3), `agent.retry_backoff_sec` (0–30, 1.0),
  `agent.summary_compaction` (true), `agent.summary_timeout_sec` (5–600, 60),
  `agent.stuck_check` (true).
- `network.mode: "online"|"web_off"|"offline"`: `web_off` disables web tools;
  `offline` also suppresses account/usage refresh and helper network use, and
  marks cloud rows unavailable. `network.allow_local_dev: string[]`: exact
  `host:port` entries (`localhost:3000`, `[::1]:8080`; `http://` accepted)
  web tools may reach although local or not on 80/443.
- `sandbox: {require: false, home_binds: string[], landlock: true}`:
  `home_binds` are home-relative paths mounted read-only in bubblewrap
  (default `.cargo .rustup .nvm .npm .cache/pip .local/bin .gitconfig .pyenv
  .bun .deno`); PUT rejects absolute paths, `..`, and anything equal to,
  inside or containing `.ssh .aws .gnupg .config .local/share .netrc .docker
  .kube .password-store .pki .azure .npmrc .pypirc .git-credentials .mozilla
  .var`. `require: true` without bubblewrap makes `exec` fail ("The command
  did not run: 'Require sandbox' is on …"); `landlock` applies when
  bubblewrap is missing.
- `network.shell: "on"|"off"|"allowlist"` (default `on`) and `network.allow`
  (at most 128 of `host`, `*.domain`, `host:port`, `[v6]:port`; without a
  port 80 and 443). The effective shell network is `off` whenever
  `permissions.network` is false or the mode is offline; Settings writes
  `permissions.network = (shell != "off")`. `allowlist` needs bubblewrap
  (fails closed without it).
- `checkpoints: {shell: true, vendor: true, keep: 1..10000 = 200,
  max_copy_files: ≤ 200000 = 5000, max_copy_bytes: ≤ 1 GiB = 64 MiB}`.
- `updates.check: bool|null` (`null` follows the packaged default).
- `spending.task_usd`, `spending.daily_usd` (number or null; defaults 1.0 and
  10.0) — see [Spending limits](#spending-limits-paid-api-models).
- `logging.level` (`error`, `warn`, `info`, `debug`; default `info`) — how
  much the engine writes to the [app log](#app-log).
- `ui`: `theme` (`system|light|dark`) and the notification switches `notify`,
  `notify_approval`, `notify_failed`, `notify_limit`, `notify_finished`
  (default true) and `notify_sound` (default false).
- `limits` ([plan limits](#plan-limits)); `local_engine` ([local models](#local-models)).

The native `exec` tool result carries `sandbox: {mode: "bubblewrap"|"landlock"|"none",
network, allow, home_read_only, home_skipped, proxy?: {reached, blocked}, …}`
and `checkpoint: {method: "git"|"copy"|"none", paths, skipped: [{path,
reason}], unavailable, ref, warning?}`, and with `only_change`,
`outside_scope? {paths, note}` or `scope_unchecked? {reason, note}`.
`agent.warning {kind: "sandbox"}` is recorded once per conversation when a
command runs without bubblewrap.

### Routing

- `GET /api/routing` → `{enabled, default, default_name, table, config,
  decisions: {[purpose]: RoutingDecision}, models}`.
- `PUT /api/routing {values: {enabled?: bool, planner?, coder?, reviewer?,
  tester?, architecture?, small_edits?, vision?, local?}}` → the same view.
  Each purpose takes a model id (at most 1024 bytes; `""`/`default` clears it);
  an explicit id must resolve to a coding model ("Choose a coding model for …").

### Onboarding, health and version

- `GET /api/onboarding` → `{completed, suggested_workspace, levels:
  ["read_only","workspace","elevated"], defaults: {permission_level:
  "workspace", permission_mode: "ask", theme: "system"}}`. Probes nothing.
- `POST /api/onboarding {workspace, permission_level?, permission_mode?:
  "ask"|"allow_edits", network?, theme?, api_key?, provider?, name?, …}` →
  `{ok, workspace, session_id}`. Trusts the folder, sets
  `permissions.level` (default `workspace`), `permissions.mode` (`ask` unless
  `allow_edits`), `permissions.network = network` (absent means false),
  `approve_shell: true` and `ui.theme` (default `light`), marks onboarding
  complete and opens a conversation. The window sends it without
  `provider`/model fields (the backend keeps its model) and also writes
  `permissions.mode` with `PUT /api/config`.
- `GET /api/health` → `{ok, app, version, workspace, model, onboarding,
  trusted, permissions, provider: {ok, name, detail}, runtime: "rust",
  cli_agents}` (+ `desktop_attached`, `desktop_pid` from the desktop).
- `GET /api/version` → `{name: "ShadowCode", version, runtime: "rust",
  transport: "native", pid}` (+ `desktop_attached`, `desktop_pid`). Local
  clients refuse an engine of another version.

### Doctor and diagnostics export

- `GET /api/doctor?test_model=true` → `{ok, version, runtime, checks: [{id,
  status: "pass"|"warn"|"fail"|"info"|"not_checked", ok, label, title?,
  detail, fix}], failures, project_map, suggestions, telemetry: false,
  diagnostic_export}`. `test_model=true` also sends one request to the model.
- `diagnostic_export = {id, filename: "shadowcode-diagnostics.json", content,
  mime: "application/json", captured_at, byte_length}`. `content` is the exact
  UTF-8 JSON that would be saved: `{schema: 1, captured_at, app, version,
  runtime, os, architecture, scope, excluded, omitted_checks, checks: [{id,
  label, status}]}`. Only reviewed check ids with fixed labels and their
  original status enter it; project maps, paths, custom and local model
  names, detail/fix text, prompts and answers never do. At most 96 checks
  (the rest counted as omitted), at most 256 KiB. The export also carries
  `runs` (the run records of up to 12 recent jobs, `{status, run}`; see
  [Run record](#run-record); model ids other than `api:openrouter:…` and
  `cli:…` are hidden because they can name local files or private hosts) and
  `log: {note, lines}`, the app log's last lines (at most 96 KiB; see
  [App log](#app-log)) with secrets redacted again and every file path
  replaced by `<path>`.
- `GET /api/diagnostic-exports/{id}` → the same snapshot, for 10 minutes; at
  most four are retained per engine. Unknown or expired ids fail ("Diagnostic
  snapshot expired; run Doctor again"); Doctor is not rerun. The desktop
  command `export_diagnostics {snapshotId}` saves only the retained bytes as a
  private file; the remote browser downloads the snapshot and refuses if it
  differs from the preview. Previewing or closing the preview writes and
  uploads nothing. This is a status export, not a crash report or a promise
  that every secret pattern is detected.

### Guardian

See [NATIVE_GUARDIAN.md](NATIVE_GUARDIAN.md). Off by default.

- `GET /api/guardian` → `{enabled, default: "off", last_run: number|null,
  last_result: object|null, pending_patch_approval, note}`.
- `POST /api/guardian/run` → `{ok, readonly: true, workspace, doctor, tests:
  {hint, present, executed: false}, wrote_main_tree: false, auto_pr: false,
  note}`. Fails when Guardian is disabled. Runs no tests.
- `POST /api/guardian/request-patch {summary}` (experimental) → `{ok,
  needs_approval: true, approval_id, summary, note}`; needs `enabled` and
  `allow_prepare_patch`.
- `POST /api/guardian/approve-patch {approval_id}` (experimental) → `{ok,
  proposal_dir, proposal, main_tree_written: false, pushed: false,
  pr_opened: false, note}`: saves `PROPOSAL.md` outside the project; the id
  works once. Despite the name, no patch is generated.

### Shell sandbox

- `GET /api/sandbox/status` → `{effective: "bubblewrap"|"landlock"|"none"|"blocked",
  bubblewrap: {installed, works, detail}, landlock_abi, network_namespace:
  {available, detail}, require, shell_network, allow, home_read_only,
  home_skipped, never_mounted}`.
- `POST /api/sandbox/discard-scratch {path}` (experimental) → `{ok,
  discarded, note}`: removes a command's managed temporary folder; only
  folders this engine process created and still tracks ("Refusing to remove
  an unregistered scratch directory").

## Accounts and models

### Picker

`GET /api/picker?refresh=1|cached=1` → `{targets: PickerTarget[],
local_engine: LocalCatalog, vendors: {[vendor]: VendorStatus}, generated_at}`.
`cached=1` reads what is known without probing vendors or fetching the
OpenRouter list (for the first paint); `refresh=1` re-probes.

```
PickerTarget = {
  id,               // "cli:codex:gpt-6-astra", "cli:cursor:auto", "cli:claude", "local:gguf:<hash>", "api:openrouter:<slug>"
  provider,         // "cli:codex"|"cli:claude"|"cli:cursor"|"cli:antigravity"|"cli:grok"|"llamacpp"|"openrouter"
  account,          // "account:codex" … | "this-computer" | ""
  model,            // exact model value the runtime accepts; "default"|"auto"
  route,            // "vendor_cli"|"local_llamacpp"|"native"
  group: "subscriptions"|"local"|"api",
  name, subtitle,
  billing,          // vendor rows: "subscription"|"api_key"|"unknown"
  inference: "cloud"|"local",
  availability: "ready"|"sign_in"|"setup_required"|"unavailable",
  availability_label: "Ready"|"Sign in"|"Setup required"|"Unavailable",
  reason,           // why not ready, or the ready detail
  featured, vision /* model AND runtime accept images */, tools /* false ⇒ "Chat only" */,
  reasoning,        // the effort control applies
  is_default, usage: UsageSnapshot,
  local?: GgufEntry // local rows
}
UsageSnapshot = {state: "ok"|"stale"|"unavailable"|"local"|"limit_reached"|"api_key", label, detail: string[],
                 plan, pool, pool_shared, windows: [{label, used_percent, remaining_percent, window_minutes, resets_at}],
                 remaining_percent, credits: {has_credits, unlimited, balance}|null, limit_reached,
                 last_refresh: number|null, provider_usage_url: string|null}
```

Rows that are not `ready` are still shown: `sign_in` opens Accounts ›
Connect, `setup_required` shows the setup hint, `unavailable` shows `reason`.
Groups show in the order subscriptions, local, api (*API keys*); without
OpenRouter rows the api group offers *Add an OpenRouter API key…*. A model
row whose usage pool reports the plan limit is `unavailable` with the usage
label as `reason`. An API-key login (Codex `auth_mode: "apiKey"`, Claude
`authMethod` other than `claude.ai`) is labelled `API key login · billed per
token` and never shows plan usage. Vendor rows carry `reasoning: false` for
models that take no effort (Claude Haiku). A loaded local row's `reason`
starts with `Loaded ·` (and says `CPU fallback (GPU load failed)` when the GPU
start failed).

### Accounts (subscriptions)

- `GET /api/accounts?refresh=1|cached=1` → `{vendors: {[vendor]:
  VendorStatus}, config: CliAgentsConfig, local_engine: LocalCatalog}`.
  Without `refresh` a vendor is probed at most once per 5 minutes; `cached=1`
  probes nothing (vendors not checked yet show `availability: "unavailable"`,
  `detail: "Not checked yet"`, persisted usage as `state: "stale"`).
  `local_engine.loaded` reflects the engine's runtime.
- `GET /api/cli-agents?refresh=1` (experimental) → `{vendors, config}`: an
  older form of `GET /api/accounts`.

```
VendorStatus = {id: "cli-<vendor>", label, product, state: "ready"|"not_logged_in"|"not_installed"|"unavailable",
                status: "pass"|"warn"|"info", availability, availability_label, detail, version, binary, fix,
                account: {email, plan, auth_mode}|null, models: [{id, label, is_default, vision}],
                accepts_images, asks_approval, fetched_at, error: string|null, usage_note: string|null,
                login_command: string[], logout_command: string[], shared_cli_note,
                billing: "subscription"|"api_key", usage: UsageSnapshot, install: AntigravityInstall|null}
AntigravityInstall = {installed, version /* "1.2.1" */, path: string|null, managed, download_bytes, installed_bytes,
                      source /* pinned dl.google.com URL */, dir, state: "not_installed"|"downloading"|"verifying"|"unpacking"|"installed"|"error",
                      busy, done, total, error: string|null}
```

`{vendor}` is `codex`, `claude`, `cursor`, `antigravity` or `grok`
("Unknown account <vendor>" otherwise).

- `POST /api/accounts/{vendor}/connect` → `{ok, state: "started"|"unsupported"|"already_running", note}`.
  Runs the official login command (`codex login`, `claude auth login`,
  `cursor-agent login`, `grok login`) with the user's environment; one login
  per vendor, stopped after 10 minutes. Progress arrives as `account.login`
  `{vendor, line, url}` (`line` redacted; `url` is the first https URL on the
  line unless it carries a code/token parameter) and completion as
  `account.login.done` after a forced re-probe. Antigravity starts the
  installed agent server with ShadowCode's private profile and relays the
  sign-in link (`initialize`, `authenticate {methodId: "oauth-personal"}`);
  it fails with the install hint when the server is not installed. Refused
  for a vendor disabled in Settings.
- `GET /api/accounts/{vendor}/login` → `{vendor, running,
  cancellation_requested, started_at?, lines: [{vendor, line, url}], done:
  {vendor, ok, detail, availability, availability_label}|null}`: buffered per
  vendor, so a reopened page can show progress. `cancellation_requested` is
  true only while a running login is being cancelled.
- `POST /api/accounts/{vendor}/cancel-login` → `{ok}` (`false` when none ran).
- `POST /api/accounts/{vendor}/disconnect {confirm: true}` → `{ok, ran:
  string[], output, note, availability, availability_label}`: runs the
  official logout (never touches credential files), forgets the cached
  status, persisted usage and every conversation's `native_session:<vendor>`,
  and re-probes. Without `confirm: true` → `{ok: false, needs_confirm: true,
  ran: [], note}`. Antigravity runs no command: it deletes ShadowCode's
  private Antigravity profile.
- `POST /api/accounts/{vendor}/refresh` → `VendorStatus` (skips the 5-minute
  freshness window, never the failure backoff).
- `GET /api/accounts/antigravity/install` → `AntigravityInstall`.
- `POST /api/accounts/antigravity/install {confirm: true}` →
  `AntigravityInstall` + `started` (`false` when a download already runs).
  Downloads in the background; size and SHA-256 are checked before
  unpacking. Without `confirm` → "Confirm the 334 MB download first";
  offline → "Offline mode: downloads are off".
- `POST /api/accounts/antigravity/uninstall` → `AntigravityInstall`; deletes
  the installed server (not the sign-in profile).

### OpenRouter (API key)

- `GET /api/openrouter` → `{key_set, key: {label, usage, limit,
  limit_remaining, is_free_tier, credits_remaining: number|null}|null,
  key_error: string|null, models, tool_models, fetched_at: number|null,
  offline, keys_url: "https://openrouter.ai/keys", activity_url:
  "https://openrouter.ai/activity"}`. The key is never returned; with a key
  set and online, the status checks it with OpenRouter's `GET /api/v1/key`
  (and `GET /api/v1/credits` for the balance).
- `POST /api/openrouter/key {api_key}` → the status. A non-empty key is
  checked first and stored as `OPENROUTER_API_KEY` in `secrets.env` (mode
  600) only if OpenRouter accepts it; the first save fetches the model list.
  `""` removes the key. Saving is refused offline ("Offline mode: OpenRouter
  is off"); removing is not.
- `POST /api/openrouter/refresh` → the status after fetching the model list.
  Refused offline.

The list (text models, from the public `GET /api/v1/models`) is cached as
`openrouter-models.json` in the state folder, fetched only once a key is
saved, and refreshed in the background by `/api/picker` when older than 6
hours. Rows (`group: "api"`): `id: "api:openrouter:<slug>"`, `provider:
"openrouter"`, `account: ""`, `route: "native"`, `inference: "cloud"`,
`vision`/`tools` from the list, `availability: "ready"` (`unavailable`
offline), `usage.state: "api_key"` with a label such as `API key · $0.15/M
in · $0.60/M out` or `API key · free`. Jobs run on the native loop
(`route: "native_http"`); the context limit comes from the list, capped at
200 000 tokens.

### Where keys are kept

API keys ShadowCode saves (`OPENROUTER_API_KEY`, remote notification tokens,
MCP server secrets) live in `secrets.env` (mode 600) unless the user moves
one to the desktop keyring (the freedesktop Secret Service: GNOME Keyring,
KWallet, KeePassXC). `config/keyring.json` lists the moved names; reads try
the environment, then the keyring for those names, then the file. ShadowCode
never unlocks the keyring itself: a locked keyring makes the key unavailable
until it is unlocked.

- `GET /api/secrets` → `{keyring: {available, detail: string|null}, file,
  keys: [{name, place: "file"|"keyring"}]}`. No values.
- `POST /api/secrets/move {name, to: "keyring"|"file"}` → the same. The key
  is written to the new place and read back before it is removed from the
  old one; on any failure it stays where it was.
- Remote access refuses `/api/secrets…`.

### Allowance

`GET /api/allowance?refresh=1` → `{generated_at, rows}`, one row per source,
from reported data only (`native/core/src/allowance.rs`):

```
Row = {id: "cli:<vendor>"|"openrouter"|"local", kind: "subscription"|"api_key"|"local", product,
       state: "ok"|"low"|"limit_reached"|"unknown"|"sign_in"|"not_installed"|"unavailable"|"offline"|"no_key"|"none",
       headline, remaining_percent: number|null, windows: [{label, remaining_percent, resets_at}],
       plan, note, last_checked, usage_url,
       used?, limit?, limit_remaining?,          // openrouter, USD
       ready_models?, on_limit?, fallback?: {id, name}|null}   // local
```

`low` means 10% or less left.

### Model registry

- `GET /api/models?detect=false&refresh=1` → `{models, cli_agents, picker,
  local_engine}`: registry rows (`{id, name, provider, endpoint,
  context_limit, metadata, …}`) plus vendor rows; `picker`/`cli_agents` are
  the picker's `targets`/`vendors`. Detection of running local servers runs
  unless `detect=false`.
- `POST /api/models/register {id} | {provider, name?, model?, endpoint?,
  api_key_env?, keep_alive?, context_limit?, id?}` → `{ok, model}`: adds a
  model configuration to the registry. `POST /api/models/select` (same body)
  also makes it `config.model`. An id already bound to another target is
  refused.
- `POST /api/models/test {id} | {provider, …}` → `{ok, reply, usage?,
  latency_ms, model?, capabilities?, error?}` after one short completion
  (45 s limit). Vendor CLIs answer their install/login state instead
  (`{ok, reply, error, vendor, state, latency_ms: 0}`). A `local:gguf:` id is
  loaded first and refused like `load` rather than waiting for another task's model.
- `GET /api/providers` (experimental) → `{providers}`: provider presets with
  `running` from detection. `GET /api/providers/detect?refresh=1`
  (experimental) → `{providers}`: local servers found (cached 30 s).

### Local models

`GET /api/local-models` → `LocalCatalog`:

```
LocalCatalog = {
  hardware: {cpu_cores, ram_bytes, gpu: string|null, vram_bytes: number|null, backend: "vulkan"|"cpu"|"unknown", devices: string[], detail},
  runtime: {state: "ready"|"setup_required"|"unavailable", path, origin: "bundled"|"managed"|"other", version, backend, commit, detail},
  models: GgufEntry[],
  loaded: {id, name, port, since, context_tokens, backend, cpu_fallback, fallback_reason: string|null, fallback_out_of_memory, vision, in_use, provenance}|null,
  ollama_store: {path, available, models: [{tag, path, projector, bytes, compatible, reason, already_added}]}
}
GgufEntry = {id: "local:gguf:<hash>", name, path, bytes, source: "file"|"directory"|"ollama"|"download",
             architecture: string|null, context_train: number|null, context_tokens, compatible, reason,
             vision, mmproj: string|null, tools, tools_reason,
             tools_basis: "unknown"|"known_template_profile"|"template_hint"|"no_template_hint"|"no_template",
             memory: {weights_bytes, kv_cache_bytes, compute_bytes, projector_bytes, overhead_bytes, total_bytes, context_tokens},
             fits: "gpu"|"cpu"|"no", availability: "ready"|"setup_required"|"unavailable",
             last_error: string|null, thinking_switch /* template has enable_thinking (sent as false) */}
```

- `runtime.state` is `ready` only after `<llama-server> --version` succeeded
  (cached per binary path, size and mtime); `hardware` comes from the same
  binary's `--list-devices` and `/proc/meminfo` (no `nvidia-smi`). Resolution
  order: `local_engine.llama_binary`, the bundled runtime next to the
  executable when the managed copy is missing or older, `~/.local/lib/shadowcode`,
  the bundle, `SHADOWCODE_LLAMA_SERVER`; never `llama-cli`, never a bare PATH lookup.
- `context_tokens` is the one context number: the server's `--ctx-size` and
  the engine's limit (min(trained, `local_engine.context_size` default 16384),
  halved until the estimate fits VRAM, else RAM; never below 4096 unless the
  model is smaller). `tools` comes from the chat template (`false` ⇒ "Chat
  only": no tool schemas are sent). `vision` is a paired projector; after load
  it is what `/props` reports. Projector, vocabulary-only and embedding GGUFs
  are not listed. `source: "download"` rows are finished catalog downloads in
  `<data>/local-models`.
- `POST /api/local-models/add {path}` (file or folder) → `{ok, local_engine}`;
  projector/vocabulary/embedding files are refused with the reason.
- `POST /api/local-models/remove {id}|{path}` → `{ok, deleted_weights: false,
  detail, local_engine}`; a file found through a folder is added to
  `local_engine.excluded`. A `download` row is refused ("… Choose Delete …").
- `POST /api/local-models/import-ollama {tag, root?}` → `{ok, local_engine}`:
  adds the blob paths (never copies or writes the store). `root` (absolute)
  defaults to `OLLAMA_MODELS`, the systemd user unit's `OLLAMA_MODELS`, then
  `~/.ollama/models`. Incompatible tags are refused with the reason.
- `POST /api/local-models/load {id}` → `{ok, loaded}`: starts llama-server
  (one model at a time; the same model is shared). Refused at once with "A
  running task is using the local model …" or "Another local model is loading".
- `POST /api/local-models/unload` → `{ok, unloaded}`; also aborts a load;
  refused while a task holds the model.
- `POST /api/jobs` on a `local:gguf:` row without vision refuses `images`
  before a job is created.

The managed server binds 127.0.0.1 on a free port with `--no-webui --jinja
--parallel 1`, `--mmproj` when paired, `-ngl 999` when the plan fits the GPU
(one retry with `--device none -ngl 0`), and a fresh 32-byte hex key per
launch passed as `LLAMA_API_KEY` in its environment (never in argv, never
stored); clients use it as a bearer token without a proxy. Config
(`config.yaml`): `local_engine: {directories, files, imports: [{path, mmproj,
name, source}], excluded, llama_binary, context_size}`.

Downloads (`native/core/src/local_downloads.rs`):

- `GET /api/local-models/downloads` → `{directory, free_bytes: number|null,
  offline, hardware: {ram_bytes, vram_bytes, gpu}, recommended: string|null,
  recommended_fit: "gpu"|"cpu"|"tight"|null, busy, models: [{id, name,
  publisher, summary, file, bytes, sha256, license, license_url, source_url,
  quantization, architecture, memory_bytes, min_memory_bytes, fit:
  "gpu"|"cpu"|"tight"|"no", recommended, supported, unsupported_reason,
  state: "available"|"downloading"|"checking"|"paused"|"failed"|"installed",
  done, total, bytes_per_second, error, model_id: string|null, path: string|null}]}`.
- `POST /api/local-models/downloads/start {id}` → the catalog; starts or
  resumes (HTTP range) one download. Refused offline, for an architecture the
  runtime lacks, when already downloaded, while another runs, and when free
  space is short. A running download re-reads the network mode every 2 s and
  stops (`failed`, partial file kept) when offline mode is turned on.
- `POST /api/local-models/downloads/pause {id}` keeps the partial file;
  `…/cancel {id}` stops and deletes it (and clears a failure); `…/delete {id}`
  unloads the model if loaded (refused while a task uses it) and deletes the
  file. Each returns the catalog.

### Local runtime receipts

`local.runtime_ready` records, per task, `runtime.provenance` (schema 1) and
a `request_policy`. The receipt is recorded after preparation and the effort
override and stays on the task; a Compare lane's receipt is cleared when a
new turn has none yet. Old receipts are never rebuilt from current settings.

- `identity_kind: "filesystem_metadata"`: model, runtime and projector
  canonical paths, byte counts, nanosecond modification times, and Unix
  device/inode/change times (device and inode as decimal strings, exact in
  JavaScript). Snapshots, not weight hashes or an immutability guarantee;
  runtime identity covers the resolved executable, not every library.
- GGUF provenance: architecture, format version, file type and quantization
  version, tensor-type counts, SHA-256 and size of the parsed header (the
  weights are excluded) and of the embedded chat template (the whole string,
  even when only a prefix is kept for display). Runtime provenance: reported
  version/commit, template identity, an allowlist of reported sampling
  defaults and reported tool capabilities. Missing observations stay absent.
- Context records requested and reported tokens; GPU fields record the
  requested mode, the actual launch mode and the detected backend (not layer
  offload); CPU fallback is recorded explicitly. Request policy states
  runtime sampling defaults, no sampling overrides, per-request response
  limits and the template thinking override when present; it does not claim
  identical sampling across models.
- Runtime reuse requires equal files and launch settings; changes during
  preparation, waiting or loading refuse the lease. An incompatible same-id
  request while the model is leased fails promptly; different models wait
  (cancellably). Metadata probes may finish after cancellation but never
  launch a server afterwards.

## Extensions

Installing or enabling an extension names the project (`workspace`, which
must be the selected project) and the reviewed content `hash`, so a stale
screen cannot change another project.

- `GET /api/plugins` → `{format: "native-plugins-v1", workspace, trusted,
  read_only, installed: PluginEntry[], available: PluginEntry[], issues,
  legacy}`; `PluginEntry = {name, version, description, hash, state?:
  "prepared"|"installed"|"removing", files: [{path, kind, hash, content?,
  status?, error?}]}`.
- `POST /api/plugins/preview {name}|{bundle}` → `{bundle: {name, version,
  description}, hash, files}`.
- `POST /api/plugins/install {workspace, bundle|name, hash}` and
  `POST /api/plugins/remove {workspace, name, hash}` → `{result: {name,
  retained?: [{path, reason}]}, catalog}`. Need a trusted, writable project.
  Installing runs nothing. Records `plugin.installation`.
- `GET /api/mcp/servers` → `{format: "native-mcp-v1", servers: [{id, name,
  hash, description, transport: "stdio"|"http", command, url, timeout_sec,
  env_names, env_refs, api_key_env, enabled}], approved: [{workspace, server,
  hash}], issues, workspace, trusted, dirs}`.
- `POST /api/mcp/servers {definition, hash?}` (register or update a
  config-defined server) and `POST /api/mcp/servers/delete {server, hash}` →
  the MCP catalog; records `mcp.registration`.
- `POST /api/mcp/activation {workspace, server, hash, enabled}` → the MCP
  catalog. `enabled` is required. Enabling checks the server may start in
  this project; activation is pinned to `hash`. Records `mcp.activation`.
- `GET /api/hooks` → `{hooks: [{name, events, builtin, command?, description?,
  path?, hash?, enabled?, timeout_sec?, path_suffix?}], dirs, issues?, format?,
  workspace?, trusted?, approved?: [{path, hash}]}`.
- `POST /api/hooks/activation {workspace, path, hash, enabled}` → the hook
  catalog; enabling is refused in read-only mode. Records `hook.activation`.
- `POST /api/sqlite {path, sql?, params?, limit?, timeout_ms?}` (strict) →
  `{ok, path, columns, rows, truncated, limit, read_only: true}`: read-only
  inspection of a SQLite file in the project ([NATIVE_SQLITE.md](NATIVE_SQLITE.md));
  without `sql` it lists tables. `sql` at most 64 000 bytes, `limit` default
  200, `timeout_ms` default 5000; blobs appear as `{type: "blob", hex, bytes}`.

## Workspace and files

All routes act on the selected project.

- `GET /api/workspace/status` → `{workspace, model, permissions, onboarding,
  routing, trusted}`.
- `GET /api/workspace/files?path=` → `{entries: [{name, path, type:
  "file"|"dir"}], workspace, path, parent}` (`parent` is `""` at the root).
- `GET /api/workspace/file?path=` → `{path, content, hash, bytes, truncated,
  secret_target}`: text up to 200 000 characters. `&full=true` → the whole
  file (at most 4 MB, UTF-8, not binary) with `truncated: false`.
  `&head=true` → `{path, hash, bytes}` only. "File not found" for a missing
  file; `hash` is the SHA-256 of the bytes on disk.
- `PUT /api/workspace/file?path= {content, expected_hash}` → `{path, hash,
  bytes}`. `expected_hash` is `"missing"` (create) or the 64-hex hash read
  earlier; any other value, or a file that changed, refuses the write. Needs
  a trusted, writable project ("This project is in read-only mode", "Trust
  this project before changing files or running commands") and no running
  task in it ("Stop the running task before making manual changes"); the
  same checks apply to `PUT` instructions and skills, `exec`, and saving the
  project map below. One exception: a save is accepted while a subscription
  turn is the project's only unfinished task and runs between its
  checkpoints; the answer then has `during_turn: true` and the save is noted
  for that turn (`native_meta` `turn_edits:<task_id>`), so a rewind keeps it.
- `GET /api/workspace/instructions` → `{exists, content, path:
  ".shadow/instructions.md"}`; `PUT … {content}` → `{ok: true}`.
- `GET /api/workspace/skills` → `{skills, issues}`; `PUT … {name, content,
  expected_hash?}` → `{ok: true}`: writes `.shadow/skills/<name>.md`
  (`name` 1–80 of `[A-Za-z0-9_-]`; content must parse as a skill).
- `POST /api/workspace/attach {filename, text}` → `{path:
  ".shadow/attachments/<id>-<name>", kind: "text"}` (name at most 200 bytes).
- `POST /api/workspace/attach-image {filename, data_base64}` → `{path, mime,
  bytes, kind: "image"}`: PNG, JPEG or WebP up to 4 MB. Both attach routes
  work in read-only projects; trust is required.
- `GET /api/workspace/mentions?q=&limit=` → `{items: [{path, kind:
  "file"|"dir"}], truncated}` (default 30, max 100): fuzzy matches (letters
  in order; file names, word starts and runs rank higher), skipping
  `.gitignore`d and hidden entries; an empty `q` lists the shallowest
  entries. The listing is cached 5 s per project.
- `POST /api/workspace/context-preview {mentions: [{path, kind}]}` (strict) →
  `{items: [{path, kind, included, reason, bytes, total_bytes, from_line,
  to_line, entries, truncated}], included_bytes, estimated_tokens,
  truncated}`. Returns no file contents; applies the same 64 KiB per file and
  256 KiB total bounds as a native task; folders attach a bounded list of
  names. `estimated_tokens` is a byte-based estimate. It is a snapshot (the
  task re-reads at start) and does not cover vendor CLI context or the repo map.
- `POST /api/workspace/exec {command, timeout?, session_id?, workspace?}` →
  `{ok, stdout, stderr, exit_code, timed_out, cancelled, truncated,
  duration_ms, pid, command}`: the terminal Run button. `command` at most
  64 000 bytes; `timeout` seconds (default 60, clamped to
  `1..agent.tool_timeout_sec`). Needs a trusted, writable project; the
  permission policy applies. Records `terminal.completed` (redacted).
- `GET /api/workspace/understand` → `{ok, project_map, text, saved: false,
  memory_file: null}`; `POST /api/workspace/understand {save: true}` also
  merges the map into `.shadow/memory/project.md` (`saved: true`).
- `GET /api/workspace/why?path=&count=` → `{ok, path, count, log,
  empty_history, diff, truncated, note}`: the last `count` (1–50, default 8)
  commits touching `path` and its current diff (the `GET /api/workspace/diff` shape).

### Editor recovery drafts

The desktop keeps acknowledged unsaved drafts in its private profile
database, separate from the project and its Git index. Refused over remote
access. Every request names the selected project's canonical path; a request
queued across a project switch fails instead of writing under the new one.

- `GET /api/workspace/editor-drafts?workspace=` → `{workspace, drafts:
  [{path, base, draft, base_hash, revision, updated_at}]}`.
- `PUT /api/workspace/editor-draft?path= {workspace, base, draft, base_hash,
  expected_revision}` → the saved draft. `base_hash` is the SHA-256 of `base`;
  `expected_revision` is `"missing"` for creation or the 32-hex revision from
  the last read/write; another revision refuses the write.
- `DELETE /api/workspace/editor-draft?path= {workspace, expected_revision}` →
  `{removed: true}`; a changed revision refuses deletion.

Paths use the writable-path validation. Only dirty UTF-8 text is stored:
base and draft each up to 4 MB, at most 32 drafts and 32 MB per project.
The window shows whether its latest recovery copy finished saving; input
still in flight at a crash is not claimed as durable. Actual saves still use
`PUT /api/workspace/file` with the disk hash and the usual trust, permission
and reservation checks.

### Workspace mutation guarantees

- File saves, instructions, skills, attachments, editor drafts, project-map
  saves and Git hunk/stage/commit bind to the selected project before waiting
  for admission; a project switch during the wait rejects the stale request
  ("Project selection changed; retry …").
- They serialize with Compare's project admission; Git workspaces also take
  the repository advisory lock, so another ShadowCode process cannot change
  these bytes while Compare snapshots or applies. Compare also refuses to
  snapshot while any persisted recovery draft differs from its base.
- On Unix, writes resolve each destination parent through pinned directory
  handles and reject symlink traversal; atomic writes, conflict hashes, move,
  delete, directory creation, mode changes and empty-directory removal are
  handle-relative; opening an existing leaf directory refuses symlinks;
  invalid move sources or stale writes are rejected before creating missing
  parents. This is not an atomic compare-and-swap against a non-cooperating
  writer (a leaf can change between the check and the rename or unlink).
  Concurrent replacement of a real directory and non-Unix behavior are
  outside this contract.

## Git and forge

Git runs with hooks, fsmonitor and external diff/textconv drivers disabled;
reads never run the repository's filter drivers, while staging and commits
keep them. Push, `gh` and `glab` use the user's sign-in environment (SSH
agent, askpass, credential helpers, `GH_TOKEN`/`GITLAB_TOKEN`) with prompts
disabled. No credential is read or stored; remote URLs are reported without
user-info; tool errors are redacted.

### Changes view

- `GET /api/workspace/git` → `{repo: true, status, porcelain, log, diff: "",
  files: [{path, label, index, work, original_path?}], truncated}` or
  `{repo: false, status: "", log: "", diff: "", files: [], error}`.
- `GET /api/workspace/diff?path=` → `{path, diff, staged, hunks, staged_hunks,
  untracked, binary, truncated}`; `hunks: [{header, lines: [{kind:
  "add"|"del"|"ctx"|"meta", text}]}]`. A new untracked file is diffed against `/dev/null`.
- `POST /api/workspace/diffstat {paths: string[]}` (at most 200) → `{stats:
  {[path]: {add, del}|null}}`: unstaged plus staged lines; every line of a new
  untracked file. `null` for binary files, new files over 4 MB, unreadable or
  symlinked new files, and every path outside a Git repository; unchanged
  paths count `{add: 0, del: 0}`. Keys are the paths as given; the project
  folder itself and outside paths are refused.
- `POST /api/workspace/diff/hunk {path, hunk, action: "accept"|"reject"}` →
  `{ok, action, path}`: `accept` stages the hunk, `reject` reverses it in the
  working tree. `hunk` must equal a hunk of the current diff ("This diff has
  changed…"); new, binary or truncated files are staged as a whole.
- `POST /api/workspace/git/add {paths}` (1–200) → `{ok: true}`.
- `POST /api/workspace/git/commit {message, allow_secrets?, hooks?:
  "run"|"skip"}` (1–32 000 bytes) → `{ok: true, hooks_ran}`; never signed.
  Before committing, and without `allow_secrets: true`, the staged changes
  are checked for secrets (provider keys, private key blocks, `.env` and
  other secret files, long random values assigned to names like `API_KEY` or
  `PASSWORD`); findings answer in band with `{ok: false, status: 409,
  secrets: [{path, line|null, kind, preview}], secrets_truncated, error}`
  and nothing is committed. `preview` is the first characters and the
  length, never the value. A line containing `shadowcode:allow-secret`, and
  paths matching a glob in `.shadowcode/secret-scan-ignore`, are skipped.
  When the project has its own commit hooks (`pre-commit`,
  `prepare-commit-msg`, `commit-msg`, `post-commit`, including a
  `core.hooksPath` such as `.husky`) and no choice was saved, the answer is
  `{ok: false, status: 409, needs_hooks_choice: true, hooks: [{name, path,
  preview}], error}`; `hooks` saves the choice for the project (`native_meta`
  `git_hooks:<project>`). Hooks never run otherwise; a hook that fails stops
  the commit with its output.
- `POST /api/workspace/git/unstage {paths}` (1–200) → `{ok: true}`: out of the
  next commit, working tree unchanged.
- `POST /api/workspace/git/ignore {path}` → `{ok: true, path}`: adds `/<path>`
  to the project's `.gitignore` and takes the file out of the index.
- `GET /api/workspace/git/hooks` → `{workspace, hooks, run: bool|null}`;
  `POST /api/workspace/git/hooks {run: bool|null}` saves (`null` asks again
  at the next commit).

### Git panel

- `GET /api/git?remote=` → `{repo: true, branch: string|null, detached,
  has_commits, upstream: "origin/x"|null, ahead, behind, staged, changed,
  branches: [{name, upstream, current}], remotes: [{name, info:
  RemoteInfo|null}], remote, remote_info, bases: string[], default_base}` or
  `{repo: false}`. `RemoteInfo = {host, path /* owner/repo */, web_url, kind:
  "github"|"gitlab"|"other"}`. The remote is the requested one, else the
  branch's upstream remote, else `origin`, else the first.
- `POST /api/git/branch {name, create?}` → `{ok, branch, created}`: switches
  to (or creates from HEAD) a validated branch name (no spaces, control
  characters, `~^:?*[\`, `..`, `@{`, `//`, leading `-` or `/`, trailing `/`,
  `.` or `.lock`, components starting with `.`; then `git check-ref-format
  --branch`). Needs a trusted, writable, idle project.
- `POST /api/git/suggest {kind: "commit"|"pr", base?, remote?}` → `{kind,
  source: "model"|"local"|"summary", model, note, message}` (commit) or
  `{…, title, body}` (pr). Commit drafts read the staged diff (an error when
  nothing is staged); PR drafts read the commits and diff since `base`
  (default: the remote's HEAD branch, else main/master/trunk/develop).
  `model` uses the conversation's picker target when ShadowCode runs it
  (local GGUF, API, OpenRouter; not subscriptions; only loopback models
  offline), `local` the loaded local model, `summary` a deterministic text.
  Secret-looking paths are listed without contents; text is redacted before
  it leaves; 60 s limit; failures fall back to `summary` with a `note`.
- `POST /api/git/push` and `POST /api/git/pr` accept `allow_secrets`: without
  it, the commits the push would send (after the upstream, or on no branch
  of the remote) are checked for secrets first and findings answer in band as
  for commits (each with its `commit`); nothing is pushed.
- `POST /api/git/push {remote?, allow_secrets?}` → `{ok, remote, branch, output, remote_info}`:
  pushes `refs/heads/<branch>` to the same name with `--set-upstream`, never
  forced; 180 s limit. Sign-in and rejection failures explain the next step.
- `GET /api/git/pr?remote=&base=` → `{remote, provider, remote_info, cli:
  {name: "gh"|"glab"|null, installed, version, authenticated, detail,
  install_url, login_command}, base, compare_url, pr: {number, url, state,
  draft, title, base}|null}` (`{remote, provider: null, cli: {name: null},
  pr: null}` without a recognizable remote). `authenticated` comes from
  `gh|glab auth status --hostname <host>`.
- `POST /api/git/pr {title, body, base, draft?, remote?}` → `{ok, url,
  number, provider, pushed, branch, base, draft}`. `title` 1–256 characters,
  `body` at most 60 000 bytes. Pushes first when the branch has no upstream
  or unpushed commits, then runs `gh pr create` (`glab mr create` for
  GitLab). Refused on the base branch and when the CLI is missing or signed out.
- `GET /api/git/pr/checks?number=&remote=` → `{supported: true, checks:
  [{name, workflow, state, bucket: "pass"|"fail"|"pending"|"skipping", link,
  description}], summary: {[bucket]: count}, overall:
  "pass"|"fail"|"pending"|"none", url, checked_at}` from `gh pr checks
  --json`. GitLab answers `{supported: false, checks: [], summary: {}, url}`
  (the pipelines page).

### Issues

"Start from an issue" ([AUTOMATIONS.md](AUTOMATIONS.md#start-from-an-issue)),
using the Git panel's remote choice and CLI check; 60 s per call.

- `GET /api/issues?remote=&limit=` (1–100, default 30) → `{ready: true,
  remote, provider: "github"|"gitlab", remote_info, cli, issues: Issue[]}`,
  or `{ready: false, remote?, provider, cli, issues: [], reason?}` when
  issues cannot be listed (`reason` for no remote or another forge;
  otherwise `cli` says what is missing).
- `GET /api/issues/{number}?remote=` → `{provider, issue: Issue, task,
  marker, branch}`. `task` is the composer text: `marker` + title, link,
  body (at most 8000 characters) and the newest five comments (1500 each),
  quoted as a description rather than instructions. `marker` is `Resolve
  GitHub issue #<n>:` (or GitLab); the window offers a closing pull request
  when a completed task starts with it. `branch` is `issue-<n>-<slug>`.
- `Issue = {number, title, body, url, author, labels: string[], state,
  updated_at, comments: [{author, body, created_at}] /* oldest first */,
  comment_count}`. Lists carry no bodies or comments. GitLab comments come
  from `glab api …/notes` without system notes; an error there leaves them out.

## Terminals and background

### Terminals

The drawer's interactive terminals: the user's login shell on a
pseudo-terminal in the selected project, with the user's environment
(AppImage library paths removed, `TERM=xterm-256color`). They run outside
the sandbox, need no approval or trust, work while a task runs, and are
never stored or shown to a model. Terminals belong to one view; they end
with it or the app (`SIGHUP` to the shell's session, then `SIGKILL`). At most
12 per view.

```
Terminal = {id, title: "Terminal N", number, workspace, shell, cols, rows, created, exited, exit_code, cursor}
```

- `GET /api/terminals` → `{workspace, terminals: Terminal[], limits: {open:
  12, scrollback_bytes: 524288}}`.
- `POST /api/terminals {cols?, rows?}` → `Terminal`.
- `POST /api/terminals/{id}/input {data}` → `{ok: true}`; at most 64 KB per
  call; refused once the shell exited.
- `POST /api/terminals/{id}/resize {cols, rows}` → `{cols, rows}` (clamped).
- `GET /api/terminals/{id}/output?after=` → `{id, data /* base64 */, from,
  cursor, more, truncated, exited, exit_code}`. Offsets count every byte the
  terminal printed; the last 512 KB are kept, so an older `after` starts at
  the oldest kept byte with `truncated: true`. At most 256 KB per read; `more`
  asks for another.
- `POST /api/terminals/{id}/close` → `{ok: true}`.

### Background processes

Long-running project processes (dev servers, watchers) started by the user
([NATIVE_BACKGROUND.md](NATIVE_BACKGROUND.md)). A process is managed from its
own project only ("Switch to this process's project before managing it").

```
BackgroundTask = {id, name, command, cwd, status: "STARTING"|"RUNNING"|"STOPPING"|"COMPLETED"|"FAILED"|"CANCELLED",
                  pid, started_at, ended_at: number|null, exit_code: number|null, output, error, truncated,
                  session_id, origin_task_id}
```

- `GET /api/background` → `{tasks: [BackgroundTask + {output_preview_truncated}]}`:
  the project's processes, `command` clipped to 4000 bytes and `output` to its last 4000.
- `POST /api/background {name, command}` → `BackgroundTask`. `name` 1–80
  bytes, unique among the project's running processes; `command` 1–64 000
  bytes; trusted project; the permission policy applies. At most 16 active
  processes, 4 per project.
- `GET /api/background/{id}` → `BackgroundTask`.
- `POST /api/background/{id}/stop` → `BackgroundTask`.

## Preview

The drawer's Preview tab (Linux; [PREVIEW.md](PREVIEW.md) covers the proxy,
the picker script and the threat model). Refused over remote access.

- `GET /api/preview/servers` → `{workspace, servers: [{port, url, source:
  "background"|"process", listening, pid, process, command, background_id,
  background_name}]}`, sorted by port. `background`: a running background
  process of this project printed the URL (`listening` says whether the port
  is open now). `process`: a process whose working folder is inside the
  project listens on the port over loopback (`url` is
  `http://localhost:<port>/` or its specific `127.x` address). ShadowCode's
  own ports are never listed.
- `POST /api/preview/open {url, app_origin}` → `{proxy_origin, proxy_port,
  target_origin, url, target_url}`. `url` must be `http://` to `localhost`,
  `*.localhost`, `127.0.0.0/8` or `[::1]` (no credentials); `app_origin` is
  the calling window's origin (`tauri://localhost`,
  `http(s)://tauri.localhost` or an `http://` loopback origin with a port).
  Opens (or reuses, for the same target and window) a reverse proxy on
  `127.0.0.1:<proxy_port>`; `url` is the page through the proxy and
  `target_url` the page at the dev server. Refused: other hosts and schemes,
  a port this process listens on, a proxy's own port, `*`/`null` origins. At
  most 8 proxies stay open (the oldest closes). The desktop records
  `proxy_port` so the preview frame may navigate there.
- The proxy serves `GET /__shadowcode_preview__/picker.js` itself and adds it
  to uncompressed `text/html` responses; it answers `421` to any `Host`
  other than `127.0.0.1:<proxy_port>` and `502` when the dev server does not answer.
- Picker → window messages (`postMessage` to `app_origin`): `{source:
  "shadowcode-preview", version: 1, type}` with `ready`, `navigated {url,
  title}`, `picked {element: {selector, tag, role, name, text, attributes,
  outer_html, styles, box: {x, y, width, height}, ancestors, url, title,
  viewport: {width, height, dpr}}}`, `console {entries: [{level:
  "error"|"warn", message, source, url, time}]}`, `pick-cancelled`. Window →
  picker: `{source: "shadowcode-app", type}` with `hello`, `pick {on}`,
  `back`, `forward`, `reload`.

## Code intelligence

Language servers, the code index, search, the repo map and embedding models
([CODE_INTELLIGENCE.md](CODE_INTELLIGENCE.md)), for the selected project.
Downloads are refused offline.

- `GET /api/code-intel/status` →
  ```
  {config: CodeIntelSettings, config_error: string|null, offline,
   languages: [{language: "rust"|"typescript"|"python"|"go"|"c", label, enabled, available, server?, path?,
                source?: "config"|"managed"|"path", note?, install_hint?, managed_package: "typescript"|"python"|null}],
   servers: [{root, language, server, program, state: "ready"|"loading"|"backoff"|"stopped"|"idle"|"busy",
              pid, idle_sec, starts, failures, last_error}],
   managed: [{id, label, packages: ["name@version"], approx_bytes, installed, installed_bytes: number|null,
              versions: [{name, version}], path, progress: {state: "installing"|"installed"|"error", error, log}|null}],
   managed_dir, npm: {available, path, node},
   index: {files, symbols, references, chunks, languages: {[lang]: files}, max_files, total,
           complete, focus, size_bytes, persistent}|null /* see reindex below */,
   embeddings: {models: EmbeddingModel[], active: string|null, runtime: string|null,
                server: {model, pid, idle_sec}|null, coverage: {embedded, chunks}|null,
                backfill: {state: "running"|"done"|"error", embedded, error}|null}}
  CodeIntelSettings = {lsp, diagnostics_on_edit, diagnostics_wait_ms, lsp_idle_minutes, max_servers,
                       servers: {[language]: {command, args}}, repo_map_tokens, semantic_search, embedding_model}
  EmbeddingModel = {id, name, summary, bytes, license, sha256, url, dims, installed, active,
                    progress: {state: "downloading"|"verifying"|"installed"|"error", done, total, error}|null}
  ```
- `POST /api/code-intel/config {…some CodeIntelSettings}` (strict: unknown or
  mistyped fields and out-of-range values fail with "Invalid code_intel
  settings: …") → `{ok, config}`. Turning `lsp` off stops running servers.
- `POST /api/code-intel/install {package: "typescript"|"python"}` → `{ok,
  started, managed}` (npm install in the background; poll status).
  `POST /api/code-intel/uninstall {package}` → `{ok, removed, managed}`.
- `POST /api/code-intel/servers/stop` → `{ok, stopped, embedding_server_stopped}`.
- `POST /api/code-intel/embeddings/install {model}` → `{ok, started, models}`:
  downloads in the background, verifies size and SHA-256, makes the model
  active if none was chosen and embeds the project.
  `POST /api/code-intel/embeddings/remove {model}` → `{ok, removed, models}`.
- `POST /api/code-intel/reindex` → `{ok, index, embedding_started}`: scans
  in batches (at most 2,000 changed files parsed per batch) until the index
  is complete or 90 seconds passed; `index.complete` says whether more
  remains (call again). `index` (also in `GET /api/code-intel/status`) =
  `{files, symbols, references, chunks, languages, max_files, total,
  complete, focus, size_bytes, persistent}`: `total` is the number of
  indexable files the last scan found (up to 250 000), `persistent` whether
  the index is kept in the profile's cache (`$XDG_CACHE_HOME/shadow-agent/index`,
  or `<profile>/cache/index`) between runs. An index from another version, or
  a damaged one, is rebuilt.
- `POST /api/code-intel/index/focus {focus: string|null}` → `index`: scan
  only that folder of the project (null: the whole project).
- `POST /api/code-intel/index/clear` → `index`: delete the project's index;
  it is rebuilt when needed.
- `POST /api/code-intel/search {query, path?, max_hits?}` (default 10) → `{ok,
  query, mode: "bm25"|"hybrid", count, hits: [{path, start_line, end_line,
  score, preview, symbols?, bm25?, similarity?}], semantic, note}`.
- `GET /api/code-intel/repo-map?tokens=&query=` (default 1024 tokens) → `{ok,
  map, files, symbols, tokens_estimate, focus, note}`.

Native tools: edit results (`write_file`, `edit_file`, `apply_patch`) may
carry `diagnostics: {new_errors: [{path, line, column, severity, message,
source?, code?}], checked, servers?, pending?, unverified?, unavailable?,
truncated?, note}`. `repo_map {query?, paths?, max_tokens?}` and
`search_code {query, path?, max_hits?}` are read-only. `goto_definition` /
`find_references` with `path`, `line`, `column` (1-based) answer `{ok,
source: "lsp:<server>", count, truncated, locations: [{path, line, column,
preview}], note}`; without a position or a server they keep the tree-sitter
shape (plus `lsp_note`). `get_diagnostics {path}` answers `{ok, path, server,
errors, total, truncated, diagnostics, note}` or `{ok: false, pending: true,
error}` while the server loads.

## Voice

Dictation ([VOICE.md](VOICE.md)): the engine records from this computer's
default microphone and transcribes on stop. Model downloads and the
`openrouter` engine are refused offline. Remote clients cannot switch the
host microphone on (`start`, `recording`, `stop`, `cancel` are refused) but
may send their own recording to `transcribe`.

- `GET /api/voice/status` → `{config: VoiceSettings, models: [{id:
  "base.en"|"tiny.en"|"base", name, bytes, license, english_only, summary,
  installed, active, progress: {state: "downloading"|"installed"|"error",
  done, total, error}|null}], languages: [{code, name}], ready, blocked:
  string|null, cpu_supported, whisper_version, openrouter_key, offline, recording}`.
  `VoiceSettings = {engine: "local"|"openrouter", model, language: "auto"|<code>,
  openrouter_model, voice_commands, live_preview, max_seconds /* 5–600 */}`.
- `POST /api/voice/config {…some VoiceSettings}` (strict) → `{ok, config}`;
  stored as `voice:` in config.yaml.
- `POST /api/voice/models/install {model}` → `{ok, started}` (background;
  size and SHA-256 verified); `POST /api/voice/models/remove {model}` → `{ok, removed}`.
- `POST /api/voice/start` → `{ok, engine, device}`. Fails before opening the
  microphone when the engine cannot run ("No voice model is installed. Open
  Settings › Voice…", no OpenRouter key, offline), with "No microphone
  found", or "Already listening".
- `GET /api/voice/recording` → `{active: false}` or `{active: true, engine,
  device, level /* 0–1 */, seconds, max_seconds, full, error, partial}`.
  Cheap; polled while listening. `full`: the length limit was reached;
  `partial`: live preview text (local engine).
- `POST /api/voice/stop` → `{text, engine, seconds, ms, message:
  string|null}`: `text` cleaned (no `[BLANK_AUDIO]`-style markers) with voice
  commands applied; empty `text` comes with a `message`. "Not listening" when
  no recording runs.
- `POST /api/voice/cancel` → `{ok, cancelled}`; discards the audio.
- `POST /api/voice/transcribe {audio}` → the `stop` shape, for a base64 WAV
  recorded elsewhere (PCM 8/16/24/32-bit or float; at most about 6 MB).

## Remote access

Settings › Remote access and `shadowcode remote` ([REMOTE.md](REMOTE.md)).
The `/api/remote*` routes answer the desktop window and local CLI clients
only.

```
RemoteStatus = {enabled, running, address, port, bound: string|null /* ip:port */, url: string|null, public_url,
                exposed /* saved address is not loopback */, allow_terminals, error: string|null,
                addresses: [{address, interface, kind: "loopback"|"tailscale"|"lan"}],
                devices: [{id, name, created_at, last_seen: number|null}],
                ntfy: {server, topic, details, events: {approval, finished, failed, limit}, token_saved, configured, error}}
```

- `GET /api/remote` → `RemoteStatus`; never includes tokens or digests.
- `PUT /api/remote {enabled?, address?, port?, public_url?, allow_terminals?}`
  → `RemoteStatus`. `address` must be `0.0.0.0`, `::`, a loopback address or
  one of this computer's addresses (default `127.0.0.1`); `port` 1024–65535
  (default 7390); `public_url` an `http(s)://` address without credentials,
  query or fragment (`""` clears it). Turning it on (or changing the address)
  starts or restarts the server; `enabled: false` stops it. A busy port is
  reported in `error` (the switch is still saved).
- `POST /api/remote/pair {host?}` → `{link, base, expires_in, qr: {size,
  rows: ["0101…"]}}`. The link is `<base>/#pair=<code>`; the code works once
  within `expires_in` seconds (600); at most 4 unused codes. `host` picks one
  of `addresses` when listening on every address; otherwise the public
  address, then the bound one. Fails unless the server is running. At most
  32 devices.
- `POST /api/remote/devices/revoke {id}|{all: true}` → `RemoteStatus`; `all`
  also cancels unused pairing links.
- `PUT /api/remote/ntfy {server?, topic?, details?, events?: {approval?,
  finished?, failed?, limit?}, token?}` → `RemoteStatus`. Empty `server` or
  `topic` turns phone notifications off; `token` is stored as
  `SHADOWCODE_NTFY_TOKEN` in the secret store (`""` removes it).
- `POST /api/remote/ntfy/test` → `{ok: true}` after the server accepted a test message.

Phone notifications use the desktop selection (`notify::select`) with the
phone's own per-kind switches. The message is ntfy's JSON publish format
posted to the server root: `{topic, title: "<notice title> · <project>",
message, tags, priority, click?}`; `message` is generic unless `details` is
on; `click` is `<public or running address>/#session=<id>`.

### The remote access server

Pages served by the remote server itself (not `Service` routes):

| Path | Auth | Answer |
| --- | --- | --- |
| `GET /`, `/assets/…`, `/manifest.webmanifest`, icons | none | the web interface (a short HTML note when the build has none) |
| `POST /_remote/pair {code, name?}` | one-time code | `{token, device: {id, name}}`; JSON only, same-origin only; 401 for an unknown, used or expired code |
| `GET /_remote/session` | token | `{device: {id, name}, allow_terminals, version}` |
| `GET /_remote/stream` | token | `text/event-stream` (below) |
| `/api/…` | token | `Service::dispatch`, filtered by the policy |

- Tokens (`scr_…`) travel only in `Authorization: Bearer`. No cookies, no
  CORS: a request with `Origin` must come from this server's own origin
  (behind a loopback reverse proxy, `X-Forwarded-Host`); `Sec-Fetch-Site:
  cross-site|same-site` is refused; preflights get 403.
- `X-Shadow-View: <8–64 of [A-Za-z0-9_-]>` gives each browser tab its own
  navigation state (default `default-view`; at most 32 views, dropped after
  an hour idle; a dropped view's terminals end).
- The stream starts with `retry: 3000` and an untyped `event:
  shadowcode:events` `data: {}` (read everything again), then `event:
  shadowcode:events` `data: {session_id, type}` per broadcast and, only while
  terminals are allowed, `event: shadowcode:terminal` `data: {type,
  terminal_id}`. Never payloads. A lagged stream sends an untyped wake-up;
  `: keepalive` every 20 s; the stream ends when the device is revoked.

Remote policy (`remote::policy::check`), besides each route's Remote column:

- `/api/remote*`, `/api/views`, `/api/runtime`, `/api/owned-jobs` and the
  editor-draft routes: 403 "Remote access is managed in Settings › Remote
  access on the computer running ShadowCode."
- Paths longer than 4096 bytes or with `%`, `\`, NUL, empty, `.` or `..`
  segments: 403 "Invalid application command path".
- Any request whose `?path=` names a secret file (`.env`, `secrets.env`,
  keys…, after normalizing `./`, `..` and trailing `/`) is refused, as is a
  body containing the redaction placeholder or the hidden-file marker (so a
  redacted value is never written back).
- `workspace` fields, and `path` on `/api/projects*`, inside the profile's
  config, data or state folder are refused.
- Answers are redacted: secret files' `content`, `diff`, `staged`, `patch`,
  `hunks`, `staged_hunks`, `lines`, `before`, `after`, `preview`, `text` are
  replaced by `[secret file hidden over remote access]` (also when the
  handler set `secret_target`), their sections are dropped from unified
  diffs, and recognizable credentials become `[redacted secret]`.
- Everything else, including routes that start agent work, is allowed:
  agent commands still go through approvals.

## About and updates

Settings › About and the update notice (`native/core/src/updates.rs`,
[DISTRIBUTING.md](DISTRIBUTING.md#updates)). Only `?auto=1` and
`POST /api/updates/check` reach the network, and only `GET
https://api.github.com/repos/Shadowfetchapps/ShadowCode/releases/latest`
with `User-Agent: ShadowCode-update-check` and no identifiers.

- `GET /api/about` → `{name, version, commit: string|null, install: {kind:
  "appimage"|"deb"|"system"|"source"|"unknown", label}, license: {spdx,
  name, holder, notice, third_party: string|null}, links: {repository,
  release_notes, releases, license, notice, issues, user_guide}, updates:
  UpdateStatus}`. `commit` is recorded at build time; `notice` is the NOTICE
  text; `third_party` is the installed notices folder. No network access.
- `GET /api/updates?auto=1` → `UpdateStatus = {current, allowed, automatic,
  setting: boolean|null, default_on, offline, policy_message, policy_source,
  install, last_checked_at, last_attempt_at, error, latest: {version, tag,
  url, published_at, signed}|null, available, dismissed, next_step: {text,
  command, link}|null, releases_url}`. `allowed` is false when the build or a
  policy file turned checks off; `automatic` adds `updates.check`. With
  `auto=1` the daily check runs first when allowed, online and due (24 h
  after the last attempt); otherwise the saved answer is returned.
  `available`: `latest` is newer than `current`; `dismissed`: its notice was
  hidden; `next_step` depends on `install` and the policy message.
- `POST /api/updates/check` → `UpdateStatus` after asking GitHub now (not
  again within 30 s). Fails with the reason when checks are off or offline; a
  failed request is reported in `error`.
- `POST /api/updates/dismiss {version}` → `UpdateStatus`; hides the notice for
  that version until a newer one appears.

## Rules and skills

The user's profile (`~/.config/shadowcode/profile/`, or
`<--profile>/shadowcode/profile/`) merged with the selected project's own
files. Behaviour: [RULES_AND_SKILLS.md](RULES_AND_SKILLS.md). Item ids are
`profile:<path inside the profile>` or `project:<path in the project>`.

- `GET /api/rules` → `{profile: {path, exists, agents_md: {content, hash, path}},
  workspace, share_with_cli_agents, items, imports, issues, starters, limits}`.
  `items[]`: `{id, scope: "profile"|"project", source: "profile"|"import:<name>"|"project",
  kind: "rules"|"skill"|"command"|"agent", name, path, description, enabled,
  bytes, hash, overridden_by}`; `overridden_by` names the more specific
  definition used instead. `imports[]`: `{name, url, path, added_at, commit:
  {commit, short, subject, date}}` (unknown values `null`). `starters[]`:
  `{name, title, summary, installed, path}`. `limits`: `{profile_file_bytes:
  16000, profile_total_bytes: 24000, total_bytes: 48000, skill_index_entries:
  48, skill_index_bytes: 6000}`. `hash` is `missing` when there is no profile
  `AGENTS.md`.
- `PUT /api/rules/profile` `{content, expected_hash}` → `{ok, hash}`. Refused
  when the file changed since `expected_hash` (`missing` creates it) or
  `content` exceeds 64,000 bytes. Creates the profile folder (mode 700).
- `POST /api/rules/items` `{id, enabled, workspace?}` → `{ok, id, enabled}`.
  A project id needs a selected project; a `workspace` that is not the
  selected project is refused.
- `POST /api/rules/sharing` `{enabled}` → `{ok, share_with_cli_agents}`:
  whether vendor CLIs receive the rulebook.
- `GET /api/rules/preview` → `{workspace, runners: [{id, label, mechanism,
  delivered, sharing_off, preview, native_files, native_skill_folders}]}` for
  `shadowcode`, `codex`, `claude`, `cursor`, `antigravity`, `grok`. `preview`
  has the attached-context inventory shape (`items[{path, kind, included,
  reason, bytes, total_bytes, …}]`, `included_bytes`, `estimated_tokens`,
  `truncated`); `kind` is `profile-rules`, `project-rules` or `skill`. Needs a
  selected project.
- `GET /api/rules/check` → `{ok, checked, errors, warnings, infos, findings,
  profile, workspace, note}`; `findings[]`: `{severity: "error"|"warning"|"info",
  code, scope, path, name, message, fix}`. `code` is one of `front-matter`,
  `too-large`, `missing-file`, `unreadable`, `no-description`,
  `long-description`, `ignored-field`, `unsafe-content`, `duplicate-name`,
  `index-full`, `limit`, `profile`. Report only; `ok` is false when there are
  errors.
- `POST /api/rules/imports` `{url}` → `{name, url, path, commit}`. Only
  `https://`, `ssh://` and `user@host:path` addresses; a URL with a password,
  an address already imported, a ninth import or a checkout over 32 MB /
  5,000 files is refused. `POST /api/rules/imports/{name}/update` →
  `{name, changed, before, commit}` (refused when the import has hand edits).
  `DELETE /api/rules/imports/{name}` → `{ok}`.
- `GET /api/rules/export` → `{targets: [{id: "claude"|"codex", label, home,
  enabled, links: [{link, target, state: "linked"|"blocked"|"available"}],
  created}]}`. `POST /api/rules/export/{target}` → `{target, created, skipped}`;
  `DELETE /api/rules/export/{target}` → `{target, removed, kept}`.
- `GET /api/rules/starters` → `{starters}`; `POST /api/rules/starters`
  `{names}` → `{installed, skipped}` (existing folders are skipped).
- `POST /api/rules/folder` → `{path}` (creates the profile folder). The
  desktop's `open_rules_folder` command calls it and opens that path; the
  window never supplies a path.
- Remote access refuses `/api/rules/imports…`, `/api/rules/export…` and
  `/api/rules/folder`.
- Event `rules.delivered {vendor, mechanism, profile_files, project_files,
  skills, plugin_skills, bytes, estimated_tokens, truncated, hash}` for each
  vendor run that received the rulebook; `hash` is the run record's
  `rules_hash`. A failure to prepare it is an
  `agent.warning` with `kind: "rules"`; the run continues without it.
- Doctor adds the check `rules-and-skills` (`pass` or `warn`), which the
  diagnostics export keeps as *Rules and skills*.
- `GET /api/workspace/skills`, `GET /api/commands` and `GET /api/agents`
  include enabled profile definitions (`source: "profile"` or
  `"import:<name>"`, absolute `path`); switched-off ones are left out.

## Your data: backup, restore, repair, reset

Settings › Your data and `shadowcode backup|restore|reset|doctor --repair`
(rules: `native/core/src/data.rs`; routes: `native/core/src/service/data.rs`).
Every route is refused over remote access: a backup can hold API keys, and a
restore or reset replaces everything. See [DATA.md](DATA.md) for the user
view.

**Backup folder** `shadowcode-backup-<YYYYMMDD-HHMMSS>[-<reason>]/` (mode
700, files 600):

```
manifest.json
state/shadow-agent.db          // consistent copy (VACUUM INTO), user_version kept
config/config.yaml             // when present
config/secrets.env             // only with include_secrets
config/remote.json             // only with include_secrets
agents/...                     // your agent definitions (~/.config/shadowcode/agents)
state/native-plugins/...       // plugin install records
```

`Manifest`:

```
{
  format: "shadowcode-backup", format_version: 1,
  app_version: string,          // the ShadowCode that made it
  schema_version: number,       // database format of the copy
  created_at: number,
  includes_secrets: boolean,
  reason: "manual"|"before-restore"|"before-repair"|"upgrade-copy",
  files: [{path: string, bytes: number, sha256: string}],
  raw_copy: boolean             // a damaged database copied as it was (not restorable)
}
```

A backup made by a newer `format_version`, or holding a database with a
newer schema than this version reads, is refused with a message that names
the version to update to. Files a newer backup holds that this version does
not know are reported in `ignored` and not restored.

- `GET /api/data` →
  ```
  {
    folders: {config, data, state},
    database: {path, bytes, wal_bytes, schema_version: number|null, supported_schema_version},
    app_version,
    backups_folder: string,
    backups: Listed[],
    upgrade_copies: [{path, name, bytes, created_at, schema_version: number|null}],
    reset_folders: string[],       // folders an earlier reset moved aside
    kept_on_reset: string[],       // data subfolders a reset leaves in place
    pending: {kind: "restore"|"reset", requested_at, source: string|null, include_secrets}|null,
    last_operation: LastOperation|null
  }
  ```
  `Listed` = `{path, name, app_version, schema_version, created_at,
  includes_secrets, reason, bytes}`, newest first; read from each
  `manifest.json` without checking digests. `upgrade_copies` are the
  automatic `shadow-agent.pre-native-<id>.sqlite` copies made before each
  database upgrade.
- `GET /api/data/backups?folder=` → `{folder, backups: Listed[]}`
  (default folder: `<data>/backups`).
- `POST /api/data/backups {include_secrets?: boolean, folder?: string}` →
  `{path, manifest: Manifest}`. `folder` must be absolute (`~/` allowed); a
  new uniquely named folder is created inside it. A failed backup leaves no
  folder behind.
- `POST /api/data/backups/inspect {path}` → `Inspection`, changing nothing:
  ```
  {
    path, kind: "backup"|"database",
    manifest: Manifest,
    summary: {conversations, tasks, jobs, goals, automations, comparisons, last_activity: number|null}|null,
    ignored: string[],
    problems: string[],            // why it cannot be restored
    restorable: boolean
  }
  ```
  `path` is a backup folder, its `manifest.json`, or a bare database copy
  (`kind: "database"`, e.g. an upgrade copy). Checks: the manifest format,
  every file's presence, size and SHA-256, the database's integrity
  (`PRAGMA quick_check`, opened read-only and immutable) and schema version,
  and that `config.yaml` loads in this version. A folder without a manifest
  or with a damaged one is an error.
- `POST /api/data/restore {path, include_secrets?: boolean}` →
  `{scheduled: true, pending: Pending, message}`. Validates like inspect
  (an unrestorable backup is an error listing the problems), copies the
  checked files to `<state>/pending-restore/` and writes
  `<state>/pending-data-operation.json`. The next engine start (desktop,
  `shadowcode serve`, or any CLI command that opens the profile) holds the
  profile lock, re-checks the staged digests, backs up the current database,
  settings (and API keys when they are replaced) into a `before-restore`
  backup, and swaps the files in; the old write-ahead log is removed with
  the old database. API keys are restored only with `include_secrets` and
  when the backup has them. An older database is then upgraded as usual.
- `POST /api/data/reset {confirm: "reset"}` → `{scheduled: true, pending,
  message}`. At the next start everything in the three profile folders moves
  into sibling folders `<folder>.reset-<YYYYMMDD-HHMMSS>` — except
  `native.lock` and, in the data folder, `kept_on_reset` (`backups`,
  `managed-worktrees`, `parallel-worktrees`, `local-models`, `voice`,
  `code-intel`). Nothing is deleted; if a move fails, everything already
  moved is put back.
- `DELETE /api/data/pending` → `{cancelled: boolean}`.
- `POST /api/data/repair` → `{kind: "repair", ok, finished_at, checks:
  [{id, label, status: "pass"|"warn"|"fail"|"not_checked", detail}], cleared:
  string[], backup: string}`. Backs up the database first (`before-repair`),
  then runs `PRAGMA integrity_check`, counts broken references
  (`foreign_key_check`, reported, never deleted), rebuilds indexes and
  statistics (`REINDEX`, `ANALYZE`, `PRAGMA optimize`) when the database is
  healthy, merges the write-ahead log, moves regenerable cache files
  (`openrouter-models.json`) into the backup folder, and forgets in-memory
  provider and vendor-status caches. `ok: false` means damage was found:
  restore a backup.

`LastOperation` (`<state>/last-data-operation.json`) is the result of the
last restore, reset or repair: `{kind, ok, finished_at, error?}` plus, for a
restore, `source`, `restored`, `secrets_restored`,
`backup_of_previous_data`, `from_version`; for a reset, `moved_to`, `moved`,
`kept`; for a repair, the repair answer above.

## Local control socket

Handled by the control server itself (`native/core/src/control.rs`), never by
`Service::dispatch`: the desktop IPC answers them "Application command is
not available", and remote access refuses them.

- `GET /api/runtime` → `{mode: "desktop"|"server"|"command"|"tui"|"acp",
  persistent: boolean /* mode != "command" */, pid, version}`: who owns the
  engine. When no desktop or server runs, `shadowcode acp` owns it
  (`mode: "acp"`, `persistent: true`) and a desktop attaches to it.
- `POST /api/views` → first frame `{view: <32 hex>}`, then a stream of
  `{event: {type, session_id, payload?, terminal_id?}}` notifications
  (bounded hints, never transcript content; `view.lagged` after lag) until
  the client sends one byte `1`, answered `{closed: true}`. Later requests
  that carry `view` use that view's navigation state and terminals. At most
  four attached views; refused while a foreground CLI task owns the profile.
  Closing a view never cancels jobs.
- `POST /api/owned-jobs` → first frame `{owned_jobs: true}`, then one
  submission per frame: `{job: body}` (→ `POST /api/jobs`), `{test: body}`
  (→ `POST /api/jobs/test`) or `{workflow: body}` (→ `POST
  /api/commands/run`), each answered with its result; `{close: true}` →
  `{closed: true}`. `workspace` is forced to the connection's project. Jobs
  submitted here are owned by the connection: when it closes or ends
  (including EOF) its unfinished jobs are cancelled, so killing a client
  never leaves orphaned work. At most eight such connections and 64 jobs per
  connection; `workflow` accepts only model workflows (plan, review, skills,
  project workflows, `/test` without a command).
- While a foreground CLI task owns the profile (`mode: "command"`), other
  clients may only read (`GET`), answer approvals and cancel jobs.

### `shadowcode acp`

`shadowcode acp` adds no route; editors speak the Agent Client Protocol to
it ([ACP_SERVER.md](ACP_SERVER.md)) and it calls this API over the control
socket, each request scoped to the ACP session's project:

- `session/new` → `POST /api/sessions {workspace, title: ""}` after checking
  `trusted_workspaces` (`--trust` adds the folder). The ACP session id is the
  conversation id.
- `session/load` / `resume` → `GET /api/sessions/{id}?view=window` (the
  folder must match `cwd`; `execution_target` becomes the model option),
  `GET /api/jobs/current?session_id=&include_finished=true` (its `mode`:
  `plan` → plan, `review` → ask, else code), and for load
  `GET /api/sessions/{id}/events?after=&limit=1000` until exhausted.
- `session/list` → `GET /api/sessions?workspace=&limit=`.
- Model option → `GET /api/picker` (`ready` rows, cached a minute per
  project); choosing one → `POST /api/sessions/{id}/target`.
- `session/prompt` → images through `POST /api/workspace/attach-image`, then
  one owned submission `{workspace, session_id, task, model, purpose, images,
  mentions, handoff_consent, queue: true}` (resource links to project files
  become `mentions`; purpose `coder`/`planner`/`reviewer`). A `needs_consent`
  answer becomes a permission request and is resent with `handoff_consent:
  true` when allowed. Progress: `GET /api/jobs/{id}/events?after=&limit=512`
  every 100 ms; approvals: `GET /api/approvals?session_id=`, answered with
  `POST /api/approvals/{id} {session_id, decision, scope}` (`scope: "task"`
  for "allow always", offered only when the approval has a `grant`);
  `session/cancel` → `POST /api/jobs/{id}/cancel`.
- Event mapping: `model.stream` / final `model.delta` → `agent_message_chunk`
  (a final delta sends only the part not already streamed);
  `tool.started` → `tool_call`; `tool.completed` → `tool_call_update`
  (`rawOutput` up to 64 KB); `plan.updated` → `plan`; `agent.warning`,
  `agent.stuck`, `routing.selected|fallback`, `model.retry`,
  `context.compacted` → `agent_thought_chunk`; `user.message` →
  `user_message_chunk` on replay only.

## Profile folders

ShadowCode keeps its data in three folders:

| Folder | Default | Holds |
| --- | --- | --- |
| config | `~/.config/shadow-agent` | `config.yaml`, `secrets.env` (mode 600), `remote.json` |
| data | `~/.local/share/shadow-agent` | backups, managed and parallel worktrees, local-model downloads, voice and code-intelligence models, webview data |
| state | `~/.local/state/shadow-agent` | the database `shadow-agent.db`, `native.lock`, `last-workspace.txt`, caches such as `openrouter-models.json` |

`XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME` are honored when they
are absolute paths. `--profile DIR` uses `DIR/config`, `DIR/data` and
`DIR/state` instead (`native/core/src/paths.rs`). A few things live beside
the profile rather than in it: user subagent definitions in `shadowcode/agents`
next to the config folder (`~/.config/shadowcode/agents` by default), the
Antigravity agent server under `$XDG_DATA_HOME/shadowcode/antigravity-acp/<version>`,
and the managed llama.cpp runtime under `~/.local/lib/shadowcode`.

The `shadow-agent` folder names are permanent for 1.x, even though the
application is called ShadowCode: renaming them would strand every existing
profile.
