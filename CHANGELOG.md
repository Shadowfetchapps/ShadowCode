# Changelog

## 1.0.0: Easy to start, ready for real work

For people starting out:
- **Free models first.** Until a subscription is connected, the picker lists
  the free models on this computer first (*Free · your code stays on this
  computer*).
- **What went wrong, in plain words.** A task that fails for a common reason
  (key refused, out of credits, conversation too long, unknown model, rate
  limit, provider trouble, local model not running, offline) explains it and
  offers the next step: **Try again**, **Continue on another model…**,
  **Choose a model** or **Open Local models**.
- **Words you'll see.** Help (`?`) explains worktree, checkpoint, rewind,
  context, tokens, hunk and more; some words in the app show their meaning
  when you point at them.
- **Approvals you can read.** Every approval card says in one sentence what
  the action does, how much it can affect (*Read-only* to *Needs admin*) and
  whether Rewind can undo it. A real shell parser reads pipelines, `&&`
  lists, `$(…)`, heredocs, `sudo` and `bash -c`; subscriptions' requests are
  explained the same way.

Safety and trust:
- **Always allow here** for exact test, build and lint commands, per project
  and revocable; never for anything that deletes, installs, uses the network
  or rewrites history.
- **Secret checks** before every commit, push and pull request from
  ShadowCode (**Remove from commit**, **Add to .gitignore**, **Commit
  anyway**); the agent's own commits never include a staged secret.
- **Project Git hooks** are asked about once per project; **new packages**
  are looked up on npm, PyPI and crates.io (does it exist, how new is it, is
  it one typo from a popular name); **lockfiles** are summarized.
- **Keys in the system keyring.** **Settings › Accounts › Where your keys
  are kept** moves API keys into the Secret Service keyring (GNOME Keyring,
  KWallet, KeePassXC) and back; the private file stays the default.
- Small Git-ignored files that are costly to lose (`.env`, local databases,
  keys) are saved before each step and restored by Rewind.
- **Heads-ups** when a task skipped or deleted tests, removed assertions,
  changed CI or switched off a check. **Only change these** keeps a task to
  the files you @-mentioned.

Real work:
- **One rulebook for every agent.** Your profile's rules, skills, commands
  and agents reach ShadowCode's agent and every subscription CLI through each
  CLI's own per-run option (**Settings › Rules & skills**, skill checker,
  starter skills, import from Git).
- **Second opinions.** Review staged changes or one task with another model
  before committing, with findings on the lines they concern and **Ask the
  agent to fix this**; **Ask another model** checks an answer. Reviewers
  never edit files.
- **Roles.** A model or subscription per step: plan, implement, review
  (**Settings › Roles**, **More › Roles**).
- **Spending limits** for paid models (ask at $1 a task and $10 a day by
  default), a price estimate before sending, *Provider busy, retrying (2 of
  5)*, **Resume at** a plan's reset time, **Try on…** another model, and
  **Run details** for every task.
- **Stuck detection** pauses a task that fails the same way three times or
  edits a file back and forth (**Keep going**, **Give a hint**, **Try
  another model**, **Stop**). `/compact [what to keep]` and `/pin` keep what
  matters through shortening; local models' malformed tool calls are
  repaired.
- **Large projects.** The code index is kept between runs, covers up to
  250,000 files, fills in progressively and can focus on one folder.
- **Review by risk.** Changed files are grouped (config and CI,
  dependencies, source, tests, generated, docs); **Explain this change**
  describes one file's change in plain words, on request.
- **Worktree tasks** can start from another branch and get setup commands,
  copied files (such as `.env`), teardown commands and their own `PORT`.
- A test caps what ShadowCode adds to each first request (system prompt and
  tool definitions) at today's size plus 10%.

Reliability:
- **Your data.** Settings › Your data (and `shadowcode backup`, `restore`,
  `repair`, `reset`) backs up, restores, repairs and starts over; upgrades
  from every release since 0.28 are tested against saved profiles, and a
  newer profile is never opened by an older version.
- A stable, checked API contract (`docs/API_CONTRACT.md`) for 1.x.
- A local model that runs out of GPU memory offers a smaller context or
  another model; a task that fails before any output ends cleanly with its
  reason; subscriptions' usage is kept when a turn fails; Rewind of a
  subscription turn keeps the files you saved during it; a window render
  error shows a reload screen.
- A rotating, redacted app log is included in health reports.
- The real-window test runs in a private Wayland session.

## 0.34.2: Ready for everyone

For people starting out:
- **Start with no account.** If no model is ready after you open a project,
  the first run offers a free model to run on this computer (recommended for
  your memory and graphics card, with its size and download time), an
  OpenRouter key, or a subscription.
- **Free models built in.** Settings › Local models lists five Apache-2.0
  models, from Granite 4.2 3B (2.1 GB) to Qwen3.6 35B-A3B (19 GB), each
  pinned to an exact Hugging Face file and checked with SHA-256. Downloads
  start only on click, can be paused and resumed (even after a restart),
  check disk space first and stop in offline mode. gpt-oss now runs from its
  upstream file.
- **Updates you can see.** At most once a day ShadowCode asks GitHub for the
  newest release (no identifiers sent) and shows a quiet notice with the right
  way to update. New **Settings › About** shows version, commit, install
  type, license and NOTICE. Distributions can switch the check off in
  `/etc/shadowcode/policy.yaml` or at build time.

Everyday use:
- **A calmer composer.** Reasoning effort, Compare and Worktree move under
  **More** (Escape or an outside click closes it); `Ctrl+Shift+Enter` still
  starts a worktree task. Unavailable options say why.
- **Polish everywhere.** Errors read as plain sentences with **Try again**;
  Escape closes every dialog and shortcuts no longer act behind one; the
  status bar says "Not connected" when the engine is down; the drawer's
  **Tasks** panel has keyboard-reachable Rename, Fork, Export and a confirmed
  Delete; a sidebar hidden by a narrow window comes back; `/new` keeps typed
  text until the new task opens. Every screen passes axe in light and dark.
- Visible task summaries refresh their check results together in one batched
  request after edits.

Vendors, verified live on current releases (Codex 0.158.0, Claude Code
2.1.284, Cursor Agent 2026.09.26, Grok 1.0.41, Antigravity agent server
1.2.1):
- Claude Code lists its real models, honours effort through `--effort` and
  shows its 5-hour and weekly usage in Allowance.
- Codex applies effort on follow-ups and no longer warns on every resume;
  Grok switches models on resumed conversations and takes an effort setting;
  a Cursor "Upgrade your plan" reply ends as a plan limit; replies no longer
  run together after a tool call.
- OpenRouter shows your account balance and explains refused requests with
  the provider's own message.

Reliability (each fix has a regression test):
- Rewind and Undo never delete files that existed before a task, and rewind
  restores permission changes.
- Automations, write subagents and cleanup keep any worktree that still holds
  work; subagent usage counts toward the parent task and subagent
  conversations are deleted with their parent.
- Long answers from slow local models are no longer cut off after ten
  minutes, and a CPU-only machine may take up to 15 minutes to start
  answering a long prompt; a taken port no longer moves a model to the CPU;
  local tasks in different projects no longer block each other.
- Compare lanes never continue on another model after a plan limit; hung
  language servers, runtime wrapper scripts, stalled downloads, background
  save errors, remote address changes and cancelled sign-ins can no longer
  hang or leave processes behind. `shadowcode acp` follows progress through
  push events; the voice model is freed after five idle minutes.

Security and privacy:
- Secret files can no longer be read through odd spellings or symlinks; keys
  are redacted from stored terminal, slash-command and hook output; @-mentions
  skip secret files.
- "Allow for this task" no longer covers wrappers, interpreters or
  `git -c`; ShadowCode never runs a repository's own Git filters and stays out
  of nested repositories.
- Remote access answers only its own host names (no DNS rebinding) and every
  API family has an explicit remote-access decision; the Landlock fallback
  restricts Unix sockets and signals; the network allow-list judges
  IPv4-mapped IPv6 correctly; a local conversation never starts a cloud
  subagent. `cargo audit` and `npm audit` report no known vulnerabilities.

Packaging:
- The Debian package passes lintian with no errors or warnings: copyright and
  NOTICE, changelog, man page, bash/zsh/fish completions (also
  `shadowcode completions`), icons at every size, correct dependencies, and
  purging it leaves your data in place. `docs/DISTRIBUTING.md` explains how
  to verify, ship and configure ShadowCode.
- Richer AppStream metadata, validated strictly on every package build.
- The AppImage build uses the pinned, source-built runtime instead of
  upstream's newest.

## 0.34.1: Failed release attempt (unpublished)

[Release run 36507326107](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36507326107)
at `206fd03` passed `native-source` but stopped in the required
`native-window` gate. On a clean CI host no model is ready, so the welcome
offers a free model, an OpenRouter key or a subscription instead of
"Choose a model"; the window test only knew the second case. The test now
checks both. Nothing was signed or published; the `v0.34.1` tag stays
unchanged, and 0.34.2 ships the same changes.

## 0.34.0: Failed release attempt (unpublished)

[Release run 36499751141](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36499751141)
at `3409d59` stopped in the required `native-source` gate: every Rust test
passed, but two new opt-in network tests (the Hugging Face catalog check and
a multi-GB download) were not on the gate's list of allowed optional tests,
and the gate refuses any unlisted skip. Nothing was signed or published. The
`v0.34.0` tag stays unchanged.

## 0.33.1: Reliability and daily workflow

- Runtime builds and notices refreshes preserve tracked source pins and reference metadata; actual build provenance stays in generated runtime output. Regression coverage reproduces the source-integrity failure that stopped the unpublished 0.33.0 attempt.
- Code highlighting, line numbers, undo/redo and recoverable editor drafts, with revision-checked saves and newline preservation.
- Bounded attached-context inventory with exclusions, truncation notices and approximate token counts.
- **Run a check…** from completed tasks: explicit commands, existing approvals, fresh output and verification receipts, no model selection or extra model turn, and preserved composer drafts.
- Independent provider sign-in recovery after navigation; cancellation remains **Stopping…** until acknowledged. Cached account state survives uncertain progress reads, and refresh waits for ongoing sign-ins.
- Safer Compare snapshots and recovery, unsaved-draft guards, and coordination of short workspace writes across cooperating ShadowCode processes.
- Shared native event listeners prevent per-task listener growth. A real source-built X11 endurance run passed 100 tasks after bounded warm-up; this is not GPU or physical Wayland endurance evidence.
- AppImage launch preserves an explicitly selected display backend; package normalization accepts only the exact expected linuxdeploy version field.
- Authenticated installer recovery and staged release verification are strengthened. [Version 0.33.1](https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v0.33.1) was published on September 28, 2026 from `e15c448`, after all 15 required build gates and protected signing passed. The workflow publication job failed on a transient draft-listing response; publication resumed locally with the same frozen publisher and signed artifact, with all seven remote assets compared before publication. See the release notes for the exact evidence and remaining limits.

See [release notes](docs/RELEASE_NOTES.md) for scope and remaining qualification.

## 0.33.0: Failed release attempt (unpublished)

[Release run 36414378697](https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36414378697) at `9e385cb` failed the managed-runtime source-integrity gate after the runtime built and its six tests passed. The builder rewrote tracked runtime pin/COMMIT timestamps. Signing and publication were skipped. The existing `v0.33.0` tag remains unchanged; the separately qualified successor is 0.33.1. Earlier local 0.33.0 package and latency results remain evidence for those exact bytes only.

## 0.32.0: The big upgrade

Everyday use:
- **Composer.** `@` searches project files and folders (local and OpenRouter
  models read their contents; subscription CLIs get `@path`) and lists
  subagents. ↑ recalls earlier prompts, a Code / Plan / Ask switch sets the
  mode, and an effort control appears for models that support one (OpenRouter
  `reasoning.effort`, Codex `model_reasoning_effort`, Claude Code thinking
  budget, local thinking switch).
- **Message actions.** Edit & resend (a fork, optionally rewinding later file
  changes), Retry and Copy on your messages; Copy and Fork on answers.
- **Approvals show the change.** File changes show a diff (or the new file),
  commands show the full command and folder. "Allow for this task" grants the
  same kind of action until the task ends (never for chained, `sudo`,
  deleting or history-rewriting commands); "Deny with note" sends a reason.
- **Per-task Review.** A full-width view of just the files a task changed,
  unified or split, with Keep / Undo per hunk and per file and Undo all.
- **Safer rewind.** Rewind asks first, marks the conversation and can be
  undone. It now also covers files changed by shell commands and by
  subscription CLIs (Codex, Claude Code, Cursor, Grok, Antigravity), through
  hidden checkpoint commits that never touch your index.
- **Parallel tasks.** **Worktree** next to Send (Ctrl+Shift+Enter) runs a
  task in its own copy of the project; Apply (checked first, conflicts
  listed), Keep as branch or Discard when it's done.
- **Notifications** for approvals (with a warning before one expires),
  failures, plan limits and finished tasks; clicking one opens the
  conversation. Sidebar badges for running, needs approval, failed and
  unread; a right-click menu; Alt+↑/↓ and Ctrl+Tab.
- **Context and cost chip** for every model, e.g. `42% · 38k / 128k · $0.12`,
  with a breakdown on click.
- **Real terminal.** Your own login shell on a pseudo-terminal (xterm.js),
  with tabs, kept while you switch tabs or the agent works; never shown to a
  model.
- **Git tab.** Create or switch branches, an editable suggested commit
  message, push with your own sign-in, and pull requests through `gh` or
  `glab` with live CI checks (or the web compare page).
- **Tools tab** in the drawer for Goals, Background processes, Worktrees,
  Automations and Issues; Settings › Advanced keeps configuration only.
- **Preview.** The drawer's Preview tab finds the project's dev servers and
  shows them through a private loopback proxy (phone, tablet and desktop
  widths; hot reload kept). **Pick element** sends an element's selector,
  text, role, size, key styles and HTML to your next message; page console
  errors can be attached too. Unavailable over remote access.
- **Voice input.** Hold the mic button (or Ctrl+Shift+Space) to dictate into
  the composer with whisper.cpp on this computer; models (74–141 MB) download
  only on request. OpenRouter transcription is optional and off by default.

A smarter agent (ShadowCode's own loop for local, OpenRouter and API models):
- **Subagents.** `spawn_agent` runs read-only `explore`, `plan` and `review`
  helpers, or `general`, which edits an isolated worktree and returns a diff
  the agent applies with your approval; up to four at once. `@name` runs
  one. Agent definitions from `.shadow/agents`, `.claude/agents`,
  `.opencode/agent`.
- **Reads other tools' files:** `CLAUDE.md`, nested `AGENTS.md`/`CLAUDE.md`,
  Cursor rules, Claude Code commands and skills; the agent loads skills on
  its own. MCP tools are offered directly as `mcp__server__tool`, and enabled
  servers are passed to Codex, Claude Code, Cursor, Grok and Antigravity.
- **Code intelligence.** Persistent language servers (rust-analyzer,
  TypeScript, Pyright, gopls, clangd) report the errors an edit introduced;
  go-to-definition and references; tree-sitter index for Python, Go, C, C++,
  Java and JavaScript; a ranked repo map; `search_code` with keyword (BM25)
  and optional embedding search through the bundled llama.cpp.
- **Long conversations** are summarized by the model instead of cut; prompt
  caching for Claude and Gemini on OpenRouter; per-task tokens and cost;
  retries on rate limits, overloads and dropped streams (never repeating a
  tool); full tool descriptions for large-context models.

Safety:
- **Real shell sandbox.** Commands see an empty home folder with read-only
  toolchains; SSH keys, cloud credentials, `~/.config` and ShadowCode's keys
  are never visible; only the project is writable. "Require sandbox" refuses
  commands without bubblewrap; otherwise Landlock applies with a warning.
  Shell network can be off, on, or limited to an allow-list enforced by a
  private network namespace and filtering proxy.

Beyond the window:
- **Remote access.** Settings › Remote access (or `shadowcode serve
  --remote`) serves the interface to a phone or another computer: off by
  default, loopback unless you choose an address, QR pairing with a key per
  device, secrets hidden. Terminals work remotely only if you allow them;
  the host computer's microphone never does. Optional ntfy phone
  notifications.
- **Editors.** `shadowcode acp` runs ShadowCode as an Agent Client Protocol
  agent for Zed, JetBrains and other ACP editors, with its models, modes and
  approvals.
- **Automations.** Scheduled prompts (hourly, daily, weekdays, weekly or
  cron) in fresh worktrees, with catch-up, history and cost. "Start from an
  issue" turns a GitHub or GitLab issue into a task and a closing pull
  request. A GitHub Action runs a checksum-verified release headless from an
  issue comment or label (`shadowcode run --approval approve` for disposable
  runners).

Under the hood:
- The application router is split into per-domain modules with typed
  requests; database work no longer blocks async workers; Compare state is
  updated transactionally.
- The window is split into hooks and components, reads one push feed instead
  of polling, keeps drawer state per project, and the light theme is really
  tested.

## 0.31.1: Apache License 2.0

- **License changed from MIT to Apache-2.0.** A new `NOTICE` file credits
  Shadowfetch as ShadowCode's original creator; copies and derivative works
  must carry it (section 4(d)). The AppImage and deb ship it as
  `ShadowCode-NOTICE` next to `ShadowCode-LICENSE`, and the package check
  requires it. Releases up to 0.31.0 remain available under MIT.

## 0.31.0: Allowance, keep going on a local model, Compare

- **Allowance.** A status-bar button and panel (`GET /api/allowance`) list
  each subscription's reported usage windows and reset times, OpenRouter
  credits left on the key, and local models ready to run. Only reported
  figures; states are ok, low (10% or less), limit reached, usage not
  reported, not signed in, not installed, offline and no key.
- **Keep going on a local model.** When a subscription reports its plan limit,
  the same conversation continues on a local model (`limits.on_limit`:
  `local`, the default, or `ask`; `limits.fallback_model` picks the model,
  otherwise the last local model used in the project, then the first ready
  one with tools). The conversation says "Codex reached its plan limit.
  Continuing on qwen3:14b on this computer." or why it could not. ShadowCode never retries on the same plan or buys more usage.
- **Compare.** `POST /api/compare` runs one task on 2 or 3 models, each in a
  managed worktree from a snapshot of HEAD plus uncommitted work (taken with a
  temporary index; the checkout and index are untouched). Keep applies one
  lane's diff to the working tree only after `git apply --check`; conflicting
  files are listed and nothing changes. Lanes are removed after keep or
  discard, and a per-project scoreboard counts wins. UI: **Compare** button
  next to Send, a Comparisons view with lanes side by side, lane
  conversations, and keep/discard/stop.
- Lane conversations are hidden from the sidebar and never become a project
  or the folder reopened at startup.
- Live harness: `--allowance` and `--compare <id>`.

## 0.30.2: Web for OpenRouter, key hygiene, docs

- **Web toggle for OpenRouter rows.** The 0.29.0 notes said OpenRouter tasks
  could use web tools, but the composer only showed **Web** for local rows.
  It now appears for OpenRouter rows too, and those rows no longer show the
  vendor-CLI note ("runs its own tools…"), since they run on ShadowCode's own
  loop.
- **`OPENROUTER_API_KEY` is removed from vendor CLIs.** A key set in
  ShadowCode's environment was inherited by subscription CLIs; it is now on
  the same removal list as the other provider keys.
- **Secret scan.** `scripts/check-secrets.mjs` fails on real-looking API keys,
  private keys or a tracked `.env`/`secrets.env` (and, with `--value-file`, on
  an exact value without printing it). It runs in the Checks workflow.
- **Docs** brought up to date for OpenRouter, Antigravity's agent server, the
  live test harness, data locations, API contract and test counts.

## 0.30.1: Antigravity verified live, cleaner answers

- **Antigravity prompts wait for the model switch.** Checked against the real
  server after sign-in: a prompt sent while `session/set_config_option` was
  still pending ended at once with no output. Prompts now wait for the switch
  (Cursor's `session/set_model` too). A live task ran `date +%Y` through a
  ShadowCode approval and answered; a follow-up resumed the same session.
- **Answers from subscriptions appear once.** Vendor turns now send
  `model.stream_end`, and the final result no longer repeats a reply with the
  same text, including in older conversations.
- **"Using Cursor · Auto · Cloud"** instead of "Using auto · Cloud": the model
  note names the product, as the picker does.
- **Quiet summary for answers.** A finished task that changed no files and ran
  no checks shows one line ("Finished · 7s · No files were changed.") without
  a *Review changes* button, which now appears only when there are changes.

## 0.30.0: Antigravity that asks first

- **Antigravity runs through Google's official ACP agent server** (the
  `antigravity-acp` entry in the ACP registry) instead of `agy`'s print mode.
  It now sends permission requests, so ShadowCode's approvals apply to its
  commands and edits, it accepts images, and it resumes sessions with
  `session/load`. Models come from the session's `model` config option and are
  switched with `session/set_config_option`.
- **Install from Accounts.** The server is downloaded only when you choose
  **Install** (334 MB from dl.google.com, size and SHA-256 pinned), unpacked
  into `~/.local/share/shadowcode/antigravity-acp/1.2.1`, and can be removed
  again. A built-in unzip handles the archive.
- **Private sign-in.** The server keeps its Google sign-in in a profile owned
  by ShadowCode. **Connect** opens the browser and relays the link;
  **Disconnect** deletes the profile. Status checks and tasks can't open a
  browser: a printed sign-in link marks the row *Sign in*, and a task stops
  with that message at once.
- **Clean launches.** Each launch gets its own temp directory, removed
  afterwards, and runs without Google API-key or cloud-project variables. On
  hosts without an IPv6 loopback, the server's own
  `--enforce_kernel_ipv6_support=false` switch is passed.
- Questions the server asks through the permission channel are skipped with a
  note for now.

## 0.29.0: OpenRouter, working web search

- **OpenRouter API keys.** People without a subscription can paste an
  [OpenRouter](https://openrouter.ai) key in **Settings › Accounts ›
  OpenRouter** and pick any of its text models from a new **API keys** group
  in the picker, labelled as billed per token. The key is checked with
  OpenRouter before it is saved to the profile's private `secrets.env`, and is
  never shown again. Rows show price per million tokens, *Vision* and *Chat
  only* from OpenRouter's model list. Tasks run on ShadowCode's own agent loop,
  so permissions, approvals, checkpoints, the Web toggle, image attachments and
  review all apply. Offline mode turns the rows off. The group sits below *On
  this computer*, so a search that matches both picks the free local model.
- **Web search works again.** DuckDuckGo refuses ShadowCode's honestly
  labelled requests with a bot check. `web_search` now falls back to
  Marginalia Search's public API instead of returning nothing, and reports
  both reasons if that fails too.
- **VPN DNS no longer blocks sites.** Some VPN resolvers (NordVPN) answer
  names such as `www.google.com` with an address in 192.0.0.0/24, which
  `web_fetch` refused as reserved. Only that block's special-purpose hosts are
  refused now.
- **Antigravity no longer reports an empty success.** `agy`'s headless mode
  denies any tool it would ask about, because it can't ask ShadowCode. When
  that leaves it with no answer, the task now fails and explains why.

## 0.28.1: Polish

- **Settings › Accounts is shorter.** Each card is headed by the product name
  (for example *Codex*), shows the version once, and puts the signed-in email
  and plan on one line. Ready accounts no longer repeat the status text or the
  usage reason. With
  reported rate-limit windows, the usage list is built from the structured
  snapshot, so reset times stay current and nothing appears twice. Credits
  appear only when the vendor says the account has some. Sign-in hints read
  "Choose Connect to sign in on <vendor>'s page", with the official command as
  the alternative.
- **Reset times round correctly.** A window resetting in 2 h 59 min 50 s reads
  "resets in 3h", not "2h 60m".
- **Stopped tasks are quiet.** A task you stop that changed nothing shows one
  line ("Stopped · 2s · No files were changed.") instead of a red card. A
  partial reply is kept without an "Interrupted response" label, and no
  "Needs attention: Model request cancelled" note is added.
- **A newly added local model always appears in the picker.** Overlapping
  picker refreshes could let an older, slower answer replace a newer one; only
  the newest answer is applied now.
- **Local model names come from the model.** File-based GGUF rows use the
  file's `general.name`, plus the quantization from the file name when there is
  one (for example *Qwen3 14B · Q4_K_M*). Files without a name keep the file
  name; Ollama imports keep their tag.
- **Layout fixes.** Notifications appear at the top centre, clear of the
  composer and the drawer's close button. The Settings navigation no longer
  wraps "Permissions & network". The drawer close button is a 32 px icon
  button.
- **Onboarding no longer probes providers.** `GET /api/onboarding` returns the
  folder and permission defaults only; it no longer starts local-server or
  vendor checks the window never shows.

## 0.28.0: One picker, real backends

- **One composer picker, filled from `GET /api/picker`.** It has
  **Subscriptions** (Codex, Claude Code, Cursor, Antigravity, Grok) and
  **On this computer** (GGUF) groups. Routing uses stable IDs (`cli:<vendor>[:<model>]`,
  `local:gguf:<hash>`). Rows show Local or Cloud, availability, Vision and
  Chat only badges, and usage. The selection is stored per conversation.
- **Vendor runtimes use official interfaces.** Codex runs through
  `codex app-server`. Claude Code runs through `claude -p` stream-json with
  host permission prompts. Cursor runs through `cursor-agent acp`. Antigravity
  runs through `agy --print=` stream-json (rewritten to the documented event
  protocol). Grok runs through `grok agent stdio`. Sign-in, models and image
  support come from each vendor's documented status commands or protocol
  handshakes, never from credential files. The non-existent `codex models`
  path and the fake "Codex · Auto" row are gone.
- **Accounts page.** Connect runs the official login command and relays its
  URL or device code. Disconnect runs the official logout after a
  confirmation, then clears cached status, stored usage and vendor sessions.
  Antigravity sign-in and sign-out stay inside `agy`.
- **Usage shows only reported figures.** Codex shows rate-limit windows per quota
  pool, with plan, reset times and credits when reported, including updates
  pushed during a turn. Claude Code, Cursor, Antigravity and Grok show
  *Usage unavailable* with the reason. Usage snapshots are stored in SQLite
  (schema 25, backed up before migration). API-key logins are labelled
  *API key login · billed per token*.
- **Plan limits stop the job.** When a vendor reports its plan limit, the job
  stops with the new status `limit_reached`. ShadowCode doesn't retry.
- **Provider API keys are removed from vendor CLIs.** `OPENAI_API_KEY`,
  `ANTHROPIC_API_KEY` and similar variables are removed from every vendor task,
  login and logout.
- **Native sessions resume per conversation** (`thread/resume`, `--resume`,
  `session/load`, `--conversation`).
- **Switching providers needs consent.** Switching provider mid-conversation
  sends a bounded handoff (at most 12,000 characters). Before local content or
  first images go to a cloud route, a consent dialog asks first.
- **The local engine reads GGUF metadata.** It gets the architecture, context,
  chat template and projector from the file. Compatibility comes from the
  runtime's `architectures.txt`. The memory plan sizes the context to VRAM or
  RAM and accounts for per-layer KV heads and sliding windows.
- **Bundled llama.cpp runtime.** A pinned llama.cpp (Vulkan module plus every
  x86-64 CPU variant) ships in both the AppImage and the deb, with its license
  notices. One model is loaded at a time. The server gets a per-launch key and
  has no web UI. If the GPU start fails, it retries once on the CPU. A task's
  lease prevents swapping the model mid-task. Load and Unload are in
  Settings › Local models.
- **Ollama import by reference.** Models from an existing Ollama store are
  imported without copying and without the daemon.
- **Local vision.** Vision on local models comes from the paired projector, and
  vision models get a `view_image` tool.
- **Web tools for local models.** `web_fetch` and `web_search` (DuckDuckGo
  HTML, sent as POST) block private networks. They pin DNS, re-check
  redirects, and limit time and size. Sources appear in the activity timeline.
- **Network modes** online, web off and offline. Offline starts no vendor
  process and refuses cloud jobs.
- **Permission modes.** The user-facing modes are *Ask before actions* and
  *Allow project edits*, and older configs are migrated. New installs start in
  Ask mode. Each vendor's enforcement is explained in Settings. Read-only tasks
  deny vendor approval requests automatically.
- **Safety.** Stored tool and approval events are redacted. The trust gate
  covers every entry point, goals included. Control sockets left by dead
  engines are removed at startup.
- **Interface.** A quiet welcome with three suggestions. The activity timeline
  and the task summary are built from recorded events. Settings are split into
  Accounts, Local models, Permissions & network, Appearance and Advanced.
  Skills, goals, background processes, health, MCP, plugins, hooks, worktrees
  and Guardian moved under Advanced. Dead controls were removed (mode tabs,
  vendor chip, old model chooser, thinking card, custom model dialog).
- **Installer.** `scripts/install-appimage.sh` takes the llama.cpp runtime from
  the AppImage, checks it, and swaps it into `~/.local/lib/shadowcode`
  atomically, with rollback. `SHA256SUMS` is required unless `--unverified` is
  given.
- **CI.** CI builds the managed llama.cpp before packaging and verifies the
  runtime in both packages. The release checks `ui/package.json` and the
  desktop entry against the tag.
- **Removed.** The legacy 0.19 Python harness is gone, along with its pytest
  suite, packaging and `serve-test-ui.py`. The Playwright suite now runs against
  `vite preview` with a fake engine. Historical release ledgers moved to
  `docs/archive/`.

## 0.27.0 — Managed local inference

- Local GGUF rows load through a ShadowCode-managed llama.cpp runtime
  (`~/.local/lib/shadowcode/llama-server`). The engine prefers that binary over
  PATH. It does not start Ollama or LM Studio. Removing a catalog entry still
  never deletes weights. One GGUF is loaded at a time.
- Codex, Claude Code, and Cursor now receive real image bytes on their official
  interfaces (Codex `localImage`, Claude image source blocks, ACP `image`).
  Antigravity still rejects attachments because its stream-json input does not
  document image blocks. Local GGUF vision is enabled only when an mmproj
  companion file is present.
- CPU llama.cpp is compiled from a pinned upstream commit and installed with
  the AppImage. Models are never auto-downloaded.

## 0.26.0 — Desktop coding agent

- One searchable composer picker with **Subscriptions** and **On this computer**.
  Rows are Codex, Claude Code, Cursor, Antigravity, and local GGUF/Ollama
  compatibility targets. Routing uses stable IDs, never display names.
- New Cursor ACP adapter (`cursor-agent acp`) and Antigravity `agy` stream-json
  adapter. Grok remains available but is not featured in the primary menu.
- Usage labels are truthful: unknown is "Usage unavailable", never a made-up
  percent. Local rows say they run on this computer with no subscription quota.
- Local GGUF catalog: add a file or folder you already have. Removing an entry
  does not delete weights. llama.cpp is optional and not auto-downloaded.
- Images attach only when the selected route actually supports vision. Vendor
  CLIs reject images until the adapter forwards real image bytes.
- Quieter home: four suggestion chips, compact tools control, activity timeline.
  Provider chip stack and permanent Build/Plan/Review/Test tabs are off the
  default composer. Settings leads with Accounts and Local models.

## 0.25.0 — "Supreme"

- **Welcome Banner**: redesigned empty-state with 8 smart starter cards (Build a
  feature, Fix a bug, Explain this codebase, Write tests, Review my changes,
  Plan a refactor, Security audit, Optimize performance). Each card populates
  the composer and sets the correct mode in one click.
- **Mode Tabs**: replaced the cryptic "Build/Plan/Review/Test" dropdown with
  accessible `role="tablist"` icon-tabs that show a hover description of exactly
  what each mode does.
- **Header declutter**: consolidated the "20-Min Flow" and "Open Weights" text
  pills into clean icon-only buttons — less noise, same functionality.
- **Context bar**: replaced the "Context X% · Ntokens" text in the status bar
  with a compact 48 px visual fill-bar that shows remaining context at a glance
  (turns danger-red above 80%). Full detail is available on hover.
- **Status bar cleanup**: removed the model name from the status bar (visible in
  the composer model picker) and moved the git branch inline for less clutter.
- **CSS micro-polish**: composer ring glow on focus; send-button subtle
  scale-up/down on hover/active; suggestion cards lift on hover; smooth
  `fade-up` animation on the welcome banner; improved font rendering.
- **Dark mode parity**: all new components (WelcomeBanner, ModeTabs, starter
  cards, ctx-bar) have matching dark-mode tokens and no flash-of-wrong-colour.
- **Mobile**: starter-grid collapses to 2-column below 540 px; mode tab labels
  are hidden on very small screens (icons remain).

## 0.24.0


- Vendor CLI agent backends: spawn the official Claude, Codex, or Grok CLI in a
  trusted workspace. ShadowCode never reads, stores, or proxies vendor OAuth
  tokens. Login remains `claude auth login`, `codex login`, and `grok login`.
- Codex uses `codex app-server` JSON-RPC, with `codex exec --json` only when
  app-server is unavailable. Grok uses ACP (`grok agent stdio`). Claude uses
  headless stream-json. Vendor tools and sandbox stay with the vendor; Pause
  and Steer interrupt and follow up. Rewind does not apply.
- Doctor reports not installed / not logged in / ready without printing
  credentials. The model picker groups local vs vendor agents; knobs live in
  Settings → Advanced, including a Claude adapter disable flag.
- Anthropic policy note: third-party clients may not use Pro/Max OAuth tokens
  directly (enforced 2026); driving the official `claude` binary with the
  user's login is currently tolerated but not guaranteed.

## 0.23.0

- Verification gate: the last test/build/lint command decides. A deliberately
  observed failing test before a fix no longer prevents the final passing run
  from counting as verified; `verification.summary` reports `red_green` when
  that sequence was observed. A failure after the last passing check still
  blocks it, and model prose never upgrades the claim level.
- Bug-fix policy matches whole words; prompts about a "prefix", a "fixture" or
  "debug logging" no longer receive the failing-test-first instruction.
- Pausing a queued task is rejected before the steer control is flagged. A
  rejected pause previously parked the task forever once it started running.
- Parallel plan cleanup recovers when a worker checkout was deleted by hand:
  the stale Git record is pruned, the worker is listed as missing, and every
  branch is retained. Integration checks name the missing checkout.
- `.env.example`, `.env.sample`, `.env.template`, `.env.dist` and
  `.env.defaults` are readable; their contents still pass through redaction.
- Symbol index touch removes deleted files by their normalized path.
- Steering strip: one Steer action pauses at the next boundary and records the
  instruction; the toast states the task stays paused until Resume. Removed an
  unused prop.
- A failed scratch-directory cleanup after a sandboxed shell command is
  reported in the tool result instead of discarding the command's output.
- The AppImage installer records `X-ShadowCode-GitSha` in the desktop entry
  (from `SHADOWCODE_GIT_SHA` or the checkout it runs from) and no longer
  duplicates the line on reinstall.
- User guide documents the verification gate and secret redaction; parallel
  workspace docs describe missing-checkout recovery. Version bumped across
  crates, desktop, UI, Python package, desktop entry and docs.

## 0.22.3

- Preserve the caller directory and relative project/profile paths in AppImage
  launches. Replace the generic launcher that selected the extraction directory
  and injected nonexistent Python and GStreamer paths. External Python tools
  remain usable without bundling Python.
- Verify both extraction modes, relative paths with spaces and external Python
  execution in packaged runtime checks. Install both `shadow` and `shadowcode`
  aliases from the same verified release.

## 0.22.2

- Retain the selected Cargo home when sanitizing packaging PATH, so clean
  Rustup-based runners can invoke the compiler and package manager. Add compiler
  discovery and unsafe-node-link regression checks. Includes the native 0.22
  workflow and reliability upgrade below.

## 0.22.1 — unpublished candidate

- Fix clean-build CI ordering: build the real desktop executable before running
  process-death and reconnection tests. Repeat qualification through the release
  workflow before publishing. Includes the 0.22 upgrade described below.

## 0.22.0 — unpublished candidate

- Native desktop controls for context windows, Ollama residency, response forks,
  parallel workspace preparation and combined integration checks.
- Durable, workspace-scoped parallel plans; cleanup refuses dirty or active
  checkouts and retains worker branches. Git filters and custom merge drivers
  cannot execute through these operations.
- Event forks retain completed tool context, exclude later turns and reject
  events belonging to other sessions. Original deletion preserves fork context.
- Rewind waits for a confirmed pause boundary and reserves idle workspaces;
  in-flight commands and other tasks cannot race checkpoint restoration.
  Task pause/resume controls are distinct from goal scheduling controls.
- Attached windows close promptly during owner reconnection and retain live
  event notifications across engine replacements and subsequent disconnects.
- Private AST cache with path confinement, content hashes, Unicode-safe
  signatures, deletion refresh, exact-name priority and syntactic call sites.
- Managed temporary scratch cleanup, corrected project mount order, operational
  bubblewrap probing and no automatic shell replay after an execution failure.
- Guardian state and proposal approvals scoped to profile/workspace. Diagnostics
  and draft proposals now accurately describe their limited behavior.
- Validated Ollama residency, numeric unload/indefinite wire values and saved
  routing preferences. Removed unused prompt hash computation.
- Fixed Ctrl+N after session activation and shortcut modifier overlap; improved
  model badge contrast in light, dark and dimmed dialog layouts.
- Split Markdown rendering into a separate UI bundle. Updated Happy DOM to a
  patched release and declared the supported Node minimum.
- Updated native build, packaging version, regression coverage and feature docs.

## 0.21.0

Crate, Tauri, CLI, AppImage, and Debian versions are **0.21.0**. Rebuild
packages from this tag; existing 0.20.0 AppImage/Debian filenames are stale.

- Long sessions keep an inspectable layered budget and a structured keep-list
  (intent, constraints, decisions, failures, plan). Compaction is still
  deterministic history truncation, not a model-written summary. Compact must
  not replace a request that already fits the hard 256-token reserve with a
  keep-list note that no longer fits; the 4096-token tester route was not
  raised.
- An attached desktop can reattach to a replaced engine process without
  starting jobs or replaying tools. GET/event catch-up is retried; mutating
  POST is not.
- Crash recovery classifies replay: reads may be repeated, shell and Git
  history need confirmation, file mutations and `git_reset`/`git_clean` are
  never auto-replayed. `recover_jobs` marks interrupted work and does not
  execute tools.
- Tool results that hit a byte or range budget set `truncated` and a
  model-visible note (`next_offset` on `read_file` when applicable).
- Autonomy caps are named profiles that never raise the configured limits.
  Repeated identical tool calls warn, ask for a replan, then pause. Changing
  arguments continue.
- `verification.summary` distinguishes model claim, observed tool evidence,
  and verified commands. Prose such as “tests passed” is not `verified`.
- Provider streams treat malformed JSON as an error, synthesize missing tool
  ids without inventing text, and do not execute tools on HTTP 429/500.
- Destructive Git stays behind elevated permission plus approval. Relocated
  worktrees are not guessed.
- Doctor and local stats stay on the machine. There is no product telemetry
  upload.
- Packaging constructs its own PATH and ignores host `/usr/local/bin` and
  Hermes node hijacks. The caller does not sanitize PATH.
- The live-WAL interoperability test drives the writer with system
  `python3` and stdlib `sqlite3`. It does not require Node 22 `node:sqlite`
  (Ubuntu `/usr/bin/node` is often 18).
- Shell execution is a lexical word list. **It is not an OS sandbox.**
  Checkpoints cover native file-tool changes, not arbitrary shell or Git side
  effects.
- Local models receive explicit runtime capability guidance and a read-only
  `system_info` tool for grounded OS and connected-display answers. Incomplete
  host detection is represented as unknown; greetings do not trigger tools.

## 0.20.0 — native development (unreleased)

- 0.21 autonomy (feature branch): inspectable context budgets, structured
  compaction keep-lists, replay classification, progressive loop
  warn→replan→pause, named autonomy caps that never silently kill, claim vs
  observed vs verified completion, local Doctor store metrics, engine-process
  auto-reattach that does not retry mutations, explicit tool-result
  truncation notes, and worktree recovery that refuses path guessing. Not
  released. Package version remains 0.20.0.

- Independent audit: Markdown keeps only fragment and credential-free http(s)
  links clickable; compatible streams continue unindexed tool-argument deltas
  and ignore repeated call ids/names; restart recovery writes a durable
  interrupted completion event.

- The desktop previews a very large Markdown response before rendering all of
  it, with an explicit control to expand the complete durable response.

- Desktop windows can attach to an existing headless/TUI engine with independent
  navigation and a visible shared-lifetime notice. Closing an attached window
  leaves engine-owned work running. A bounded private event feed supports
  live updates and completion notifications without an idle-window wake timer.

- Live response updates reuse unchanged Markdown messages instead of reparsing
  earlier answers and code blocks on every update. Native catch-up combines
  adjacent fragments of the same response while preserving durable cursors and
  task/completion boundaries.

- Native desktop saved-history pages support older/newer/latest navigation
  without loading thousands of events or tasks at once. Live work continues
  while browsing; original records remain available through export.

- Reviewed desktop and CLI repair restores missing managed Git connection files
  while preserving the original index and local edits, with private recovery
  journals and refusal of conflicting or lost metadata.

- Reviewed CLI and desktop copying carries staged, unstaged, binary and regular untracked
  edits into a new worktree while preserving the source. It rejects stale
  snapshots and retains partial destinations after failure.

- Native terminal input uses level-triggered polling after CI exposed dropped
  keys around resize events. Full-color PTY checks verify Escape closes Help
  before the next task submission.

- Reviewed CLI and desktop worktree return prepare incoming commits in the source index
  without committing. Stale reviews and active work are rejected; conflicts and
  ignored local files are preserved for explicit resolution.

- CI now exercises 200 native tasks, stalled-provider cancellations and output
  floods while checking memory, descriptors, child cleanup and history integrity.

- Native development: reviewed CLI and desktop rescue for missing worktrees restores retained
  commits into a new checkout while preserving the original branch, index and
  registration; missing uncommitted files are not reconstructed.

- Desktop Settings adds worktree creation, opening through project trust,
  inspection and reviewed removal. Actions bind to the displayed source project;
  dirty or stale reviews cannot authorize removal.

- Managed worktrees support inspection and reviewed clean removal. Dirty/ignored
  files, detached or locked checkouts, active tasks and background servers block
  removal. Branches and commits remain; private records are archived after success.

- Native worktree creation starts an isolated branch from a local commit while
  preserving source checkout edits. CLI inventory and private recovery records
  retain visibility into interrupted creation. The complete worktree workflow
  remains in development.

- Desktop polling uses compact recent/active job records instead of repeatedly
  transferring full saved results. Older active jobs remain visible; expanding
  queued prompts and opening conversations fetch their complete records on demand.

- CLI conversation and job IDs resolve against the full saved history instead
  of recent-list limits. Old records remain available for continuation, export,
  rename and job inspection; default conversation selection is project-scoped.
  Indexed lookup avoids loading unrelated job results just to resolve a prefix.

- Native `tui` frontend shares the Rust engine with desktop and CLI: Unicode
  input, conversation/model/project pickers, task modes, queued follow-ups,
  explicit approvals, tool cards and paged saved history. Terminal-owned workflows
  cancel on disconnect while unrelated work in an attached engine continues.
  Real PTY checks exercise submission, approval, planning and terminal restoration.

- Concurrent desktop/CLI settings changes preserve unrelated fields, project
  trust, integration grants and secret entries. Read–modify–write operations are
  serialized through the owning native engine. Rejected updates preserve the
  saved file; configuration/secret reads reject special files and remain bounded,
  and writes cannot exceed the size accepted on restart.

- Native project plugins install reviewed built-in or local JSON bundles into
  real skills, slash commands, hooks and MCP definitions. Settings and CLI show
  contents and require current project/content hashes; executable integrations
  need separate activation. Private journals support interrupted-install cleanup,
  uninstall preserves local edits, and legacy plugin directories remain intact.
  Native Python/Linux/iOS workflows replace executable Python plugin loading.

- MCP registration prints Codex, Claude Code and Cursor configuration for stdio
  or an existing authenticated HTTP gateway. Output references credential
  variables without exposing secrets; literal path encoding and option validation
  prevent accidental configuration changes. Independent official TypeScript SDK
  1.x/2.x probes exercise both transports with real native approvals and test jobs.

- Native MCP serving supports authenticated loopback Streamable HTTP alongside
  stdio. Both expose the same project-scoped tools, approvals and owned tasks.
  HTTP reconnects preserve the gateway's jobs; gateway shutdown cancels unfinished
  owned work while unrelated engine tasks continue. Mandatory credentials, host
  checks, denied browser origins, bounded traffic and protocol validation apply.
  The executable probe runs both transports against source and packaged builds.

- Rust task engine with confined file tools, bounded processes, streaming local
  and compatible models, scoped approvals, checkpoints, persisted queues, and
  recovery. Settings and legacy history are migrated with backups.
- Tauri desktop window embeds the compiled interface and communicates through
  IPC, without a Python runtime or browser launcher. Native dialogs, external
  links, window state, notifications, and managed shutdown are implemented.
- Native profile directories are private to the current account, including
  existing profiles, without deleting data or changing existing parent-directory
  permissions. Unsafe profile leaves and lock files are rejected. Notifications
  keep full text contrast throughout their entrance animation.
- Native SQLite tools, CLI and MCP queries replace the Python built-in reader,
  with parameter binding, live WAL support, confined paths, denied SQL writes
  and bounded/cancellable results. Bundled SQLite is updated to 3.53.2, including
  its WAL-reset corruption fix.
- Required task inspection recognizes successful native database reads. Small
  model contexts can use a shorter, enforced response budget while retaining
  required input; requests that still cannot fit fail before contacting the model.
- The native desktop can queue follow-up messages while the current task streams,
  show waiting work across project conversations and cancel individual queued
  tasks. Reloads select the running task first; queue cancellation cannot stop a
  task that has already begun. Sidebar activity and transcript progress now
  distinguish queued, running and completed work.
- Native goals preserve milestone results, pause and resume, require recorded
  inspection/verification for the default checklist, and stop on failed checks.
  Goal tasks appear live in the conversation; results and progress are accessible
  from the drawer.
- Native model routing adds editable Plan/Build/Review/Test selections, explicit
  task overrides, and recorded selection/fallback notices. Model registration
  distinguishes identical names on different endpoints and preserves saved
  credential references and context limits during discovery.
- Native background processes run development servers and watchers alongside
  tasks, retain bounded live logs and durable history, honor project permissions,
  and stop managed child processes when the application closes. Legacy history
  is imported without signalling stored PIDs or replaying old commands.
- Models can start project servers under shell permissions and command hooks,
  inspect bounded status/logs, and request scoped approval to stop them. Plan
  and Review retain read-only access. Background servers appear in the shared
  panel and survive coding-task completion or cancellation until explicitly
  stopped or the owning application closes, with durable task attribution.
  Background cards use readable labels and explicit start/stop prompts; approval
  headings wrap on compact windows and warning labels meet light-theme contrast.
  Following new activity now scrolls before paint, including task submission,
  so a newly added approval remains in view.
- Native slash commands run real tasks and terminal actions, persist command
  cards, and support explicit project skills with source/hash provenance. Skills
  preserve Plan/Review restrictions and queued instructions; the editor validates
  metadata and rejects stale saves. Project notes guide subsequent tasks.
- Native CLI runs tasks without a display, streams ordered event JSON, handles
  terminal and external approvals, and exposes sessions/export, checkpoints,
  goals, model settings, skills, and background controls. It shares an open
  desktop or explicit headless owner through a private Unix connection while
  keeping project selection independent. Interruptions, broken output pipes,
  and disconnected manual commands clean up their owned work.
- Native window automation covers actual tool execution, approvals, reload,
  cancellation, goal progression/pause, accessibility, and subprocess cleanup.
- Native lifecycle hooks use reviewed YAML/JSON command definitions with explicit
  project/content approval in Settings or the CLI. Command/commit gates block
  failed actions, completion checks drive bounded repair, and persisted results
  survive reload. Changed definitions require review; read-only tasks keep hooks
  inactive, and cancellation/shutdown wait for command cleanup. Legacy Python
  callbacks are surfaced for migration without importing them.
- File tools explain how to create files with `expected_hash: missing` and reject
  malformed hashes with actionable guidance. Existing read/stale-hash protections
  remain in force. Completion-check diagnostics are bounded before model repair.
- Ollama requests preserve runtime repair and compaction instructions for model
  templates that ignore later system messages. Original positions remain labelled,
  user/tool data retains its role, and saved conversation order is unchanged.
- Native AppImage and Debian build scripts preserve per-format executable
  metadata, include versioned dependency notices, and check startup, versions,
  matching compiled code, notice hashes, and absence of Python sidecars. The
  packaged AppImage runs the same real-window workflow as the debug executable.
- Source-built AppImage runtime gives every extraction-mode launch a private
  directory, forwards shutdown signals, and cleans up after payload exit.
  Concurrent CLI/window launches preserve each other's resources. The build
  retains runtime source archives, patches, notices, and compiler provenance;
  verification checks the packaged runtime code and matching source artifact.
- Manual native-window probes with installed Ollama models cover minimal coding
  edits, visible command approval, independent verification, saved continuation,
  cancellation during actual streaming, checkpoint rewind, and process shutdown.

This development branch is not yet the replacement release. Remaining
integrations, broader stress checks, packaging, and installation are tracked in
[the migration gates](docs/archive/NATIVE_MIGRATION.md).

## 0.19.0 — 2026-09-19

### Workspace

- Persistent project/task sidebar with search and pinned tasks.
- Refined light and dark surfaces, responsive composer, consistent iconography.
- Markdown responses, code-copy buttons, task plans, saved text drafts, and
  scrolling that respects the user's position.
- Accessible modal dialogs, labeled controls, keyboard navigation, reduced-motion
  support, and explicit approval buttons.
- Integrated bounded terminal and staged/unstaged review, including new files.

### Runtime and correctness

- Durable desktop job records, interrupted-run recovery, and active-run reconnect.
- ID-based SSE replay fixes duplicate prior tasks and the 800-event stall.
- Tool-call identifiers correctly correlate parallel operations.
- Session activation switches the backend workspace; follow-ups recover bounded
  conversation context and preserve custom titles.
- Cancellation waits for worker exit and releases pending approvals.
- Concurrent workspace jobs are rejected; direct edits wait for active jobs.
- Live and final plan changes are persisted and displayed.
- Git uses literal paths/NUL-delimited status; stale hunk requests are rejected.
- Cross-origin browser requests and untrusted Host headers are rejected.
- Direct workspace mutations enforce read-only mode.

### Distribution and maintenance

- Standalone Linux x86_64 AppImage and portable archive with checksums.
- Python wheels include the compiled interface; MCP is a declared dependency.
- Source installer uses an isolated venv without uninstalling system packages.
- AppImage installer validates the executable before replacing earlier builds.
- CI checks Python, TypeScript, browser workflows, and accessibility; tag workflow
  produces release assets. Updated architecture, security, usage, and release docs.

## 0.18.0

Introduced the minimal drawer-based desktop, Goal Mode, per-task rewind, session
management, model picker, notifications, and source-checkout updater. Release
history is available on [GitHub](https://github.com/Shadowfetchapps/ShadowCode/releases).
