# ShadowCode 0.33.0 — release preparation

This version improves editing, explicit verification, provider sign-in recovery and long-running desktop reliability. It is being prepared for release; this document does not establish signed publication or green remote CI. Historical notes below retain their original versions and scope.

## Everyday workflow

- **Editor.** Code highlighting, line numbers, undo/redo and recoverable drafts. Saves check the original disk revision, preserve supported newline formats and retain edits typed while a save is pending.
- **Context.** Inspect the files and bounded excerpts attached to the next task, including exclusions, truncation and approximate token counts. Estimates are not provider-reported usage.
- **Run a check…** Completed-task summaries can start a fresh, user-entered command against current project files without selecting a model or spending another model turn. Existing trust and command approvals apply. Output and a new receipt appear in a new task while your composer draft and earlier evidence remain intact. A successful command proves that check's result, not overall application correctness.
- **Accounts.** Returning to Settings restores each provider's active login independently. Cancellation shows **Stopping…** until acknowledged. Unknown progress preserves cached information instead of silently starting fresh probes. A successful login process alone does not establish Ready or subscription billing.

## Workspace and desktop reliability

- Compare refuses app-owned unsaved editor drafts before taking its snapshot. Short writes coordinate across cooperating ShadowCode processes, and interrupted cleanup retains ownership evidence and recoverable work. Arbitrary terminal commands and external applications remain outside that cooperative lock.
- Shared native event registrations avoid growth on every task. A source-built X11 run passed 100 qualification tasks after 25 warm-up tasks with stable listener, descriptor and thread counts and intact SQLite integrity.
- AppImage launch preserves an explicit display-backend choice. The package normalizer accepts linuxdeploy's single exact matching version field while rejecting unrelated launcher changes.
- Approval restoration, provider event ownership, cancellation, local-model scheduling and installer recovery have additional regression coverage.

## Release and qualification status

Release jobs separate build, signing and publication, verify private artifact snapshots and require complete scoped receipts. The AppImage installer requires publisher signatures and durable accepted-state checks; checksum overrides and `--unverified` do not bypass authentication. Previously published unsigned artifacts are not retroactively authenticated.

Recorded source evidence includes 507 UI unit tests, 75 browser scenarios and the 100-task native endurance run. These counts describe the tested source snapshots, not automatically the final 0.33.0 package. Small live coding tasks have passed across Codex, Claude, Cursor, Grok and Antigravity; full live login-expiry/cancellation/continuation qualification and ACP billing entitlement remain separate.

Physical COSMIC/Wayland interaction coverage is partial: the overall run failed its requested window-resize assertions. GPU inference endurance and broader keyboard/clipboard/IME/DPI acceptance are not established. The protected signing environment and public trust for 0.33.0 are provisioned. Final version-bound packages, the reviewed tooling pin, protected release approval and remote CI receipts must be checked before publication. See the [implementation evidence ledger](FLAGSHIP_IMPLEMENTATION.md) for exact snapshots, retained failures and later results.

Expected Linux package names are `ShadowCode_0.33.0_amd64.AppImage`, `ShadowCode_0.33.0_amd64.deb` and `ShadowCode_0.33.0_appimage-runtime-sources.tar.gz`. Use the [release procedure](RELEASING.md) and [publisher authentication contract](RELEASE_AUTHENTICATION.md); these names are not a download-availability claim.

## Published 0.32.0

ShadowCode 0.32.0 is the biggest update yet: it's easier to use every day,
the agent is smarter, commands are properly sandboxed, and you can use
ShadowCode from your phone, your editor, or on a schedule. The 0.31.1 notes
follow below.

## Easier every day

- **Type `@` to attach files** and folders, **↑** for earlier prompts, and
  switch between **Code, Plan and Ask** right in the composer. Models that
  support it get an **effort** control.
- **Edit and resend** any of your messages, or retry or copy it.
- **Approvals show the actual change:** the diff for a file, the full
  command for a shell step. **Allow for this task** stops repeat prompts,
  and **Deny with note** tells the agent why.
- **Review just what a task changed**, full width, and keep or undo each
  change. **Rewind** now asks first, can be undone, and also covers shell
  commands and Codex, Claude Code, Cursor, Grok and Antigravity edits.
- **Run tasks side by side:** **Worktree** next to Send starts a task in its
  own copy of the project; apply it, keep it as a branch or discard it.
- **Notifications** when a task needs you, fails, hits a plan limit or
  finishes; click to jump to it. The sidebar marks what needs attention.
- **See context and cost** for every model in the status bar.
- **Preview the app you're building** inside ShadowCode and click an
  element to show the agent exactly what to change.
- **A real terminal**, a **Git tab** that goes from commit to pull request
  with live CI checks, and **voice dictation** that runs on your computer.

## A smarter agent

- **Subagents** explore, plan, review or work in parallel copies of your
  project.
- **Language servers** tell the agent about errors right after each edit,
  and a **repo map** and **code search** help it find its way.
- Long conversations are **summarized** instead of cut, OpenRouter
  requests use **prompt caching**, and dropped connections are **retried**.
- It reads **CLAUDE.md, Cursor rules and Claude Code skills and commands**,
  and your MCP servers reach every model.

## Safer by default

- Shell commands run in a **sandbox that hides your home folder**: SSH
  keys, cloud credentials and API keys stay invisible, and only the project
  is writable. You can require the sandbox and limit the network to a list
  of hosts.

## Beyond the window

- **Remote access:** scan a QR code to follow and steer ShadowCode from your
  phone. Off by default; Tailscale recommended.
- **Zed and JetBrains:** `shadowcode acp` makes ShadowCode an agent inside
  your editor.
- **Automations:** run a prompt on a schedule, turn a GitHub issue into a
  task and pull request, or run ShadowCode in GitHub Actions.

## Changed in 0.31.1

- **Apache License 2.0.** ShadowCode is now Apache-2.0 licensed instead of
  MIT. A new NOTICE file credits Shadowfetch as the original creator, and
  anyone sharing a copy or a modified version must include it. Releases up to
  0.31.0 remain available under MIT.

## New in 0.31.0

- **Allowance.** One button in the status bar shows what's left everywhere:
  each subscription's usage and reset times, your OpenRouter credits, and the
  local models ready to run. Only figures the services report; nothing is
  guessed.
- **Keep going when a plan runs out.** When Codex, Claude Code or another
  subscription hits its limit, the same conversation continues on a local
  model, with a note saying so. Prefer to choose? Set **When a plan runs out**
  to **Ask me**. ShadowCode never retries on the same plan or buys more
  usage.
- **Compare.** Press **Compare** next to Send to give one task to 2 or 3
  models. Each works in its own copy of your project (uncommitted work
  included); see their changes, checks, time and cost side by side, then
  **Keep** the best one. Only the kept changes reach your files, nothing is
  committed, and the copies are cleaned up. **Wins in this project** tracks
  which model you keep. Tested live: two OpenRouter models fixed the same bug
  in 11 seconds and the kept fix applied cleanly.

## Fixed in 0.30.2

- **Web for OpenRouter.** The **Web** toggle now appears for OpenRouter
  models, as the 0.29 notes promised. Tested live: an OpenRouter model searched
  the web and cited its source.
- **Your OpenRouter key stays put.** It is stripped from the environment of
  every subscription tool ShadowCode starts, like the other provider keys.
- **No secrets in the repository.** A new check scans every tracked file for
  real-looking API keys and runs on every push.
- **Docs** for OpenRouter, Antigravity, data locations and testing are current.

## Fixed in 0.30.1

- **Antigravity works end to end.** Tested live after signing in: it lists
  its 11 Gemini models, asks ShadowCode before running a command, answers, and
  resumes the conversation. A timing bug that made the first prompt end with
  no answer is fixed.
- **Answers appear once.** Replies from Codex, Claude Code, Cursor, Grok and
  Antigravity are no longer repeated under *Result*.
- **Clearer notes.** The model note reads "Using Cursor · Auto · Cloud", and a
  question that changed nothing ends with one quiet line instead of an empty
  report card.

## New in 0.30.0

- **Antigravity asks first.** It now runs through Google's official ACP agent
  server instead of the `agy` CLI's print mode, which silently denied any
  command. Commands and edits reach ShadowCode's approvals, images work, and
  conversations resume.
- **One-time install.** **Settings › Accounts › Antigravity › Install**
  downloads the server from Google (334 MB, checksum-verified). **Connect**
  then signs in with Google in your browser. This sign-in is ShadowCode's own
  and separate from the `agy` CLI.
- **No surprise browser tabs.** Status checks and tasks never open a sign-in
  page. If the sign-in expires, the row says *Sign in* and a task stops with
  that message.

## New in 0.29.0

- **OpenRouter.** Paste an OpenRouter API key in **Settings › Accounts** and
  pick any of its text models from the picker's new **API keys** group. Rows
  show the price per million tokens and whether the model supports images or
  tools. Every token is billed to your OpenRouter account, and the group says
  so. These models run on ShadowCode's own agent loop, so permissions,
  approvals, checkpoints, web tools, images and review all work. See
  [OpenRouter](https://github.com/Shadowfetchapps/ShadowCode/blob/main/docs/OPENROUTER.md).
- **Web search.** When DuckDuckGo answers with a bot check, `web_search` asks
  Marginalia Search's public API instead of giving up.
- **VPN users.** Sites that a VPN's DNS resolves into 192.0.0.0/24 (NordVPN
  does this for Google) are no longer refused by `web_fetch`.
- **Antigravity.** Tasks where Antigravity silently denied a command now fail
  with an explanation instead of reporting success with no answer.

## Fixed in 0.28.1

- **Accounts.** Each card shows the product name, its version once, and the
  signed-in email with the plan on one line. Usage windows are listed once, with
  reset times that stay current. Credits appear only when the vendor reports
  some.
- **Reset times** round to whole minutes ("resets in 3h", not "2h 60m").
- **Stopped tasks** that changed nothing show a single quiet line instead of a
  red card and without a "Needs attention" note. The partial reply is kept.
- **Newly added local models** always appear in the picker right away.
- **Local model names** come from the GGUF file's own name, plus its
  quantization, for example *Qwen3 14B · Q4_K_M*.
- **Notifications** appear at the top centre, clear of the composer and the
  drawer. "Permissions & network" no longer wraps in Settings, and the drawer
  close button is larger.
- **First run** no longer checks local servers or vendor tools that the
  onboarding screen does not use.

## ShadowCode 0.28.0

ShadowCode 0.28.0 is a Linux desktop coding agent with one window and one
model picker. It runs your existing subscriptions through the vendors' official
command-line tools, or GGUF models on your own hardware with a llama.cpp
runtime that ships with the app.

## What changed

- **One picker** lists **Subscriptions** and **On this computer**. Every row
  shows Local or Cloud, whether it is ready (and why not), Vision and Chat only
  badges, and the usage the vendor reports. Each conversation remembers its
  model.
- **Accounts.** **Settings › Accounts** signs in and out with each vendor's
  official login and logout commands. ShadowCode never reads credential files,
  and it removes provider API keys from the environment of every vendor task,
  login and logout.
- **Usage.** Codex shows its rate-limit windows per quota pool, with reset
  times. Other vendors show *Usage unavailable* with the reason. The last
  snapshot survives a restart. When a vendor reports its plan limit, the task
  stops and you pick another model.
- **Local models.** A pinned llama.cpp (Vulkan and every x86-64 CPU level) is
  bundled in the AppImage and the deb. Models are read from their GGUF
  metadata. Context is sized to your VRAM or RAM. Models in an existing Ollama
  store can be imported by reference, without copying and without the daemon.
  Vision comes from a paired projector.
- **Web tools for local models.** `web_fetch` and `web_search` block private
  networks and cloud metadata addresses.
- **Network modes.** Online, Web tools off, Offline.
- **Permission modes.** *Ask before actions* (the default for new installs) and
  *Allow project edits*. Settings explains what each vendor enforces.
- **Switching providers mid-conversation.** A bounded summary of the earlier
  turns is handed over, and ShadowCode asks your consent before local content
  goes to a cloud provider.
- **Resuming vendor sessions.** Each vendor's own session is resumed per
  conversation.
- **Installer.** `scripts/install-appimage.sh` now requires `SHA256SUMS` and
  installs the llama.cpp runtime from the AppImage into
  `~/.local/lib/shadowcode`, rolling back if a step fails.
- **Removed.** The legacy Python harness and its tooling.

## Subscription status

| Vendor | Runtime | Image input | Approvals shown in ShadowCode | Usage |
| --- | --- | --- | --- | --- |
| Codex | `codex app-server` | Yes | Yes | Rate-limit windows per pool |
| Claude Code | `claude -p` stream-json | Yes | Yes | Unavailable |
| Cursor | `cursor-agent acp` | When advertised | Yes | Unavailable (plan tier only) |
| Antigravity | `agy --print=` stream-json | No | No (its own settings) | Unavailable |
| Grok | `grok agent stdio` (ACP) | No | Yes | Unavailable (session tokens only) |

## Local model support

- **Formats.** Any GGUF whose architecture the bundled llama.cpp supports.
  Unsupported ones (for example `gptoss` at this pin) are listed with the
  reason.
- **Hardware.** Vulkan GPUs, or any x86-64 CPU. One model is loaded at a time.
  If the GPU start fails, ShadowCode retries on the CPU and labels the row.
- **Tools and vision.** Models whose chat template has no tool support run as
  *Chat only*. Models with a paired projector accept images and get
  `view_image`.

## Known limits

- **Claude Code** was not verified end to end on the build machine, because it
  was not signed in there. The stream-json adapter is covered by unit
  tests with scripted protocol frames, and sign-in state by fake-CLI tests.
- **Antigravity** exposes no machine-readable usage. It never sends approval
  requests to ShadowCode: its own settings decide what it may do. Sign-in and
  sign-out happen only inside `agy`.
- **Cursor** reports its plan tier but no remaining allowance.
- **Grok**'s ACP interface accepts no images, and Grok has no read-only mode
  for Plan/Review.
- **`web_search`** uses DuckDuckGo's HTML page, which may block automated
  requests. The tool then reports that no results were retrieved.
- **Ollama stores outside `~/.ollama/models`** are found only through
  `OLLAMA_MODELS` or an `Environment=OLLAMA_MODELS` line in the Ollama systemd
  user unit. The system-wide service's store isn't found automatically.
- **ShadowCode is not an operating-system sandbox.** Shell commands run as
  your user.

## Downloads

- `ShadowCode_0.32.0_amd64.AppImage`
- `ShadowCode_0.32.0_amd64.deb`
- `ShadowCode_0.32.0_appimage-runtime-sources.tar.gz`
- `SHA256SUMS`

Builds are for x86_64 Linux with glibc 2.39 or newer (Ubuntu 24.04 or later).
Check the files with `sha256sum --ignore-missing -c SHA256SUMS`, then install
with `scripts/install-appimage.sh` (see the README).
