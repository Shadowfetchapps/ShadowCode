# ShadowCode architecture (0.30)

```text
React window (ui/)                 shadowcode CLI / TUI / MCP clients
      │ Tauri IPC  invoke("api")              │ private Unix socket
      └──────────────┬────────────────────────┘
                     ▼
          Service (native/core/src/service.rs)   request router: /api/…
                     ▼
          Engine (engine.rs)   jobs, queue, approvals, events, trust gate
                     ▼
          Runtime facade (runtime.rs)   Vendor(vendor) | Local
            ┌────────┴──────────────────────────┐
            ▼                                   ▼
 Vendor adapters (cli_agent/*)        Native agent loop (engine + tools/)
 codex app-server · claude -p ·       ShadowCode tools, permissions,
 cursor-agent acp ·                   checkpoints, web tools
 agy_acp_server.par (Antigravity) ·        │                 │
 grok agent stdio                          ▼                 ▼
 (vendor runs its own loop)   local_engine.rs →       OpenRouter
                              local_runtime.rs        chat completions API
                              managed llama-server    (api:openrouter:*,
                              (127.0.0.1)             openrouter.rs)
```

One executable (`shadowcode`, crate `shadowcode-desktop` in `src-tauri/`) runs the desktop window and
the CLI. The engine lives in the `shadowcode-core` crate (`native/core`) and
runs in-process with the window. There is no HTTP server between them.

## Layers

- **Desktop** (`src-tauri/src/main.rs`): hosts the React build (`ui/dist`,
  embedded) in the system WebKit webview and exposes one IPC command, `api`,
  which forwards `{method, path, body}` to the `Service`. The UI contract is
  [docs/API_CONTRACT.md](docs/API_CONTRACT.md). A window can also
  attach to an engine already running in a headless `serve` or TUI process.
  Closing an attached window leaves that engine's work running.
- **Service** (`service.rs`): routes requests (`/api/picker`,
  `/api/accounts/*`, `/api/openrouter/*`, `/api/local-models/*`, `/api/jobs`,
  sessions, review, config). `dispatch` parses a request into a `Call` and
  hands it, by the segment after `/api/`, to one route module under
  `service/` (`sessions`, `jobs`, `workspace` + `git`, `worktrees`,
  `settings`, `accounts`, `model_catalog`, `goals`, `background`,
  `extensions`, `compare`, `commands`, `memory`, `feed`). Bodies are typed structs
  whose `Text`/`Flag`/`Loose` fields read absent or mistyped values the way
  the untyped API did. Synchronous handlers run on tokio's blocking pool.
  The CLI reaches the same service through
  `control.rs`, a Unix socket in `/run/user/<uid>/shadowcode/` with
  peer-credential checks. It opens no TCP listener. At startup it removes
  sockets left behind by dead engines.
- **Engine** (`engine.rs`): owns jobs. Each workspace runs one active job plus
  queued follow-ups. `start_with_context` is the single entry point for the
  desktop, CLI, goals, MCP and workflows. It enforces workspace trust, the
  offline refusal of cloud routes, read-only mode for Plan/Review, and
  handoff consent. All of these checks run before a job row is written.
- **Runtime facade** (`runtime.rs`): a two-variant enum, `Vendor(Vendor)` or
  `Local`, that dispatches availability, model discovery, capabilities
  (vision, tools, whether approvals reach ShadowCode, cloud or local), resume,
  cancel and usage. Provider protocols stay inside the adapters.
- **Vendor adapters** (`cli_agent/`): `codex.rs` (app-server JSON-RPC, plus an
  `exec` fallback), `claude.rs` (stream-json), `acp.rs` (Cursor, Grok and
  Antigravity). `antigravity_server.rs` installs Google's ACP agent server on
  request (pinned version, size and SHA-256), prepares its private profile and
  per-launch temp directory, and builds its launch arguments. Each adapter is a
  line-oriented state machine. `runner.rs` owns the process, stdin/stdout,
  approvals, stall detection and cancellation. The vendor runs the agent loop
  with its own tools and sandbox. ShadowCode's tools are never injected into
  it.
- **Native agent loop**: used for local GGUF rows, OpenRouter rows
  (`openrouter.rs`: key check, cached model list, picker rows) and configured
  OpenAI-compatible endpoints. It handles context accounting and compaction,
  tool calls through `permissions.rs`, file checkpoints, verification, web
  tools (`web.rs`) and `view_image` for vision models.

## Catalog

`cli_agent/catalog.rs` holds one `VendorCatalog` per engine. It probes each
vendor with official interfaces only:

| Vendor | Probe |
| --- | --- |
| Codex | app-server `account/read`, `account/rateLimits/read` and `model/list` (`codex_probe.rs`) |
| Cursor, Grok | ACP `initialize`, `authenticate` and `session/new` (`acp_probe.rs`); no prompt is sent |
| Claude | `claude auth status` and the model aliases in `claude --help` |
| Antigravity | ACP `initialize`, `authenticate` (`oauth-personal`) and `session/new` against the agent server, with a no-op `BROWSER`; a printed Google sign-in link means *Sign in*. Models come from the session's `model` config option |

- **Caching.** Results are cached with a 5-minute freshness window and backoff
  on failure. *Offline* mode starts no probe.
- **Picker rows.** `picker_rows` builds one row per discovered model. The local
  catalog (`local_engine.rs`) adds GGUF rows, and `openrouter.rs` adds the
  **API keys** rows once a key is saved (from a cached model list, refreshed in
  the background when older than 6 hours). `/api/picker` returns all three.
  Row IDs are stable routing IDs: `cli:<vendor>[:<model>]`,
  `local:gguf:<hash of the canonical path>` or `api:openrouter:<slug>`.
  Display names are never used for routing.

## Usage persistence

- **Snapshots.** `cli_agent/usage.rs` models what a provider reports: limit
  windows, quota pool, plan, credits when stated, `limit_reached`, and the
  refresh time. Anything not reported is left as unknown. Codex rate limits
  come from probes and from `account/rateLimits/updated` pushes during a turn.
  Each push emits `usage.updated`.
- **Storage.** Raw official payloads go into the `usage_snapshots` table,
  keyed by vendor, account and pool. After a restart they appear as
  *Last checked …* until the next probe. A snapshot older than 30 minutes is
  marked stale. Disconnect deletes the vendor's rows.
- **Database.** SQLite `user_version` is 25 (`store.rs`). Opening an older
  database first copies it to `shadow-agent.pre-native-<id>.sqlite` (mode 600)
  with the SQLite backup API. Migrations then run forward in order inside one
  transaction. A database from a newer version is refused. The database runs
  in WAL mode behind one connection lock; async code does writes and large
  reads through `Store::run` (tokio's blocking pool), so a slow `fsync` never
  stalls a worker that is streaming a model reply.
- **Per-conversation state.** Execution targets and vendor session IDs are
  `session_meta` rows (`execution_target`, `native_session:<vendor>`). The
  per-project default is a `native_meta` row (`execution_target:<workspace>`).
  Every `native_meta` key is built in `store/keys.rs`. JSON documents there
  (compare records, per-project compare indexes and scoreboards) are changed
  only inside one `BEGIN IMMEDIATE` transaction (`Store::meta_transaction`);
  a compare record carries a revision, so a stale save from a second process
  is refused instead of overwriting it or counting a run twice.

## Local runtime lifecycle

1. **Resolve.** `local_engine::runtime_candidates` looks for `llama-server`
   in this order: the configured `local_engine.llama_binary`, then the runtime
   bundled next to the executable if it is newer than the managed copy
   (compared by the `built=` line in `COMMIT`), then `~/.local/lib/shadowcode`,
   then the bundle, then `SHADOWCODE_LLAMA_SERVER`. It never uses `llama-cli`
   or a bare `PATH` lookup.
2. **Verify.** The runtime counts as ready only after `llama-server --version`
   succeeds. Devices come from `--list-devices`, cached per binary path, size
   and mtime, plus `/proc/meminfo`.
3. **Inspect.** `gguf.rs` reads the header for architecture, trained context,
   chat template and per-layer KV geometry. `local_engine::plan_context` picks
   the largest context that fits the GPU, otherwise RAM.
4. **Load.** `local_runtime.rs` starts one server with `--host 127.0.0.1`, a
   free port, `--no-webui --jinja --ctx-size N --parallel 1`, `--mmproj` if a
   projector is paired, and `-ngl 999` if the plan fits the GPU. It passes a
   fresh 32-byte key as `LLAMA_API_KEY` in a cleared environment. The server
   runs in its own process group with `PR_SET_PDEATHSIG`. Its stderr drains
   into a 16 KB ring. Loading can be cancelled and waits up to 90 s for
   `/health`. If the server exits early on the GPU, it is retried once with
   `--device none -ngl 0`.
5. **Lease.** A task holds a lease on the loaded model. While any lease is
   held, loading another model or unloading is refused.
6. **Stop.** Unloading, swapping or shutting down sends SIGTERM to the process
   group, then SIGKILL after 5 s.

`ollama_store.rs` reads Ollama manifests and blob paths. It never writes to
the store.

## Process supervision

- **Vendor CLIs** are spawned in the workspace in their own process group with
  `PR_SET_PDEATHSIG(SIGKILL)` and `kill_on_drop`. Provider API-key variables
  are removed from their environment. The Antigravity server also runs without
  Google cloud-project variables, with `GEMINI_HOME` set to ShadowCode's
  private profile and `TMPDIR` set to a per-launch directory that is removed
  afterwards. Cancelling sends SIGTERM to the group, then SIGKILL. A run with
  no output line for `stall_timeout_sec` (default 900) fails.
- **Login and logout** commands (`cli_agent/auth.rs`) run as supervised
  children: one login per vendor, cancellable, stopped after 10 minutes.
  Antigravity's Connect starts the agent server and sends `authenticate`;
  its Disconnect deletes the private profile.
- **Probes** use short timeouts. Codex probes and doctor commands run with a
  cleared, allow-listed environment.
- **Restart recovery.** A profile lock (`native.lock`) prevents two engines
  from owning one profile. On restart, unfinished jobs are marked
  `interrupted`. Shell commands and file edits are never replayed.

## Event model

Every job writes durable events to SQLite with monotonically increasing IDs.
A bounded broadcast channel (1024 entries) only wakes up listeners. The UI
reads committed rows from its last cursor, so a missed wakeup or a reload
never duplicates or loses output.

| Event group | Events |
| --- | --- |
| Tools and approvals | `tool.started` / `tool.completed` (redacted in storage; web tools add `sources`), `approval.requested` / `approval.resolved`, `command.completed` |
| Output | `model.stream_end` (a streamed reply is complete, so the final result doesn't repeat it; vendor turns send it too) |
| Routing and handoff | `routing.selected` (with `inference: cloud\|local`), `vendor.session`, `agent.handoff`, `model.switched` |
| Usage and limits | `usage.updated`, `limit.reached` |
| Results | `checkpoint.updated`, `checkpoint.restored`, `files.changed`, `verification.summary`, `web.source` |
| Accounts (not tied to a session) | `account.login`, `account.login.done` |

Job statuses end in `completed`, `failed`, `cancelled`, `interrupted` or
`limit_reached`.

## Frontend

- **Shell.** `ui/src/App.tsx` only wires hooks to components. Behaviour is in
  `ui/src/hooks/`: `useNavigation` (startup, opening conversations and
  projects, trust), `useConversation` (the job event stream and history
  pages), `useFeed` (approvals and jobs), `useTaskActions` (sending, slash
  commands, consent, plan-limit continuation), `useJobControls`,
  `useCompare`, `useShortcuts` (one keydown listener), `useTheme`,
  `useStickyScroll` and `useDrawerMemory`. Layout is in
  `ui/src/components/shell/` (`TopBar`, `Stage`, `ChatView`,
  `TranscriptRows`, `ComposerDock`, `StatusBar`, `AppDialogs`).
- **Push, not polling.** The desktop shell forwards every engine broadcast as
  `shadowcode:events {session_id, type}`. `useFeed` reads `GET /api/feed`
  (pending approvals and the job list) only for the types it lists
  (`approval.*`, `job.changed`, `agent.*`), with a 15 s backstop, and keeps
  unchanged arrays so nothing re-renders for them.
- **Rendering.** Transcript rows carry stable keys (message, call or event
  ids, `lib/rowKeys.ts`) and are memoized per row; long pages render their
  latest 150 rows first. The elapsed timer ticks in its own component. Task
  summaries count changed lines for every file in one
  `POST /api/workspace/diffstat`.
- **Picker and settings.** `ui/src/components/UnifiedPicker.tsx` is filled only
  by `GET /api/picker`. The settings pages are in `components/settings/`:
  Accounts (vendors, the Antigravity install and the OpenRouter key), Local
  models, Permissions & network, Appearance, Advanced and About.
- **Consent and progress.** `ConsentDialog.tsx` answers the `needs_consent`
  reply. `ActivityTimeline.tsx` and `TaskSummary.tsx` are built from recorded
  events.
- **Transport.** `ui/src/lib/transport.ts` has a single transport, Tauri IPC.
  A fake engine is honoured only in builds made with
  `VITE_SHADOW_TEST_TRANSPORT=1`, which the Playwright suite uses.
  `e2e/check-bundle.mjs` checks that the production bundle doesn't contain it.
- **Markdown.** `components/Markdown.tsx` renders model output without raw
  HTML, remote images or non-http(s) links.

## Distribution

The AppImage and the deb both contain the executable with the embedded UI and
the managed llama.cpp runtime in `usr/lib/shadowcode`. The runtime uses
relative `$ORIGIN` links and `NOTICES/`. `scripts/check-native-package.mjs`
verifies both packages. `scripts/install-appimage.sh` installs the AppImage's
runtime into `~/.local/lib/shadowcode`. See [docs/RELEASING.md](docs/RELEASING.md).
`scripts/native-deb.mjs` finishes the deb for distributions (copyright,
changelog, manual page and completions from `shadowcode manpage` and
`shadowcode completions`, icons, stripped runtime, libc dependency); see
[docs/DISTRIBUTING.md](docs/DISTRIBUTING.md).

`updates.rs` serves Settings › About (`/api/about`) and the update notice
(`/api/updates`). It detects the install type from the package format Tauri's
bundler writes into the executable, the AppImage environment and the
executable's path. At most once a day, when the build, the system
policy (`/etc/shadowcode/policy.yaml`, `<prefix>/share/shadowcode/policy.yaml`),
`updates.check` and the network mode allow it, the window's
`GET /api/updates?auto=1` asks GitHub's latest-release API. The request has a
fixed User-Agent and no identifiers; the answer is kept in
`update-check.json` in the state folder. Nothing is downloaded.
