# Subagents and files from other coding agents

ShadowCode's own agent (local and OpenRouter models) can hand focused work to
**subagents**: child agents with their own context. It also reads the project
files other coding tools use, such as `CLAUDE.md`, Cursor rules, Claude Code
commands and skills, and Claude Code / opencode agent definitions.

Vendor CLIs (Codex, Claude Code, Cursor, Antigravity, Grok) run their own
agent loops, so subagents apply to ShadowCode's own agent only. Vendor CLIs do
receive the project's enabled MCP servers (see the end of this page).

## How a subagent runs

The agent starts subagents with the `spawn_agent` tool. It gives each one a
complete, self-contained task, because a subagent does not see the
conversation. It can start one, or several at once with `tasks`.

- **Read-only subagents** (`explore`, `plan`, `review`, and any definition
  without write tools) work in the project with read-only permissions. They
  can read, search and inspect Git; they cannot edit files or run commands.
- **Write subagents** (`general`, or a definition with `mode: write`) work in
  their own managed Git worktree. It starts from the project's current files,
  including uncommitted work. When the subagent finishes, ShadowCode collects
  its changes as a diff and removes the worktree. The parent agent sees the
  diff and applies it with `apply_agent_changes`. That is an ordinary patch:
  it is checkpointed, can be rewound, and asks for approval in **Ask** mode.
  Write subagents need a Git repository with at least one commit. Binary
  changes are listed but not applied. If the changes cannot be collected
  (for example, the diff is too large), the worktree is kept and its path
  is given, so nothing is lost; find it in **Tools › Worktrees**. A subagent
  still running when ShadowCode stopped is shown as *Interrupted* after the
  next start.
- **Approvals.** When a subagent needs approval (a shell command, or an edit
  in its worktree in **Ask** mode), the card appears in the parent
  conversation. Its reason starts with `Subagent <name>:`.
- **Stop.** Stopping the parent task stops its subagents.
- **Limits.** At most 4 subagents run at the same time, and one task can start
  16 in total. Subagents cannot start their own subagents. A subagent stops
  after its step limit (the definition's `max_turns`, or 24).
- **Models.** A subagent uses the parent task's model unless its definition or
  the call names another model id from the picker. Subscription CLIs cannot
  be subagent models. Only one local GGUF model runs at a time, so a subagent
  on a different local model fails while the parent holds its own.

Each subagent run appears in the conversation as a card. Click it to see the
task, the result, changed files and token use. **Open transcript** shows the
subagent's own conversation. Subagent conversations are hidden from the
sidebar. A subagent's tokens and cost also count toward the parent task's
usage (and its conversation's total and token budget). Deleting the parent
conversation deletes its subagent conversations and their saved diffs; if a
fork of the conversation still shows a subagent's card, that subagent's
conversation moves to the fork instead.

## Asking for an agent

Start a message with `@name` to run that agent first, for example
`@explore where is the config file loaded?`. The mention is removed from the
task the subagent receives, and the main agent then continues with its
result. `@agent-name` (Claude Code's form) works too. Typing `@` as the first
character of a message lists the available agents. A mention must start the
message or follow a space; `@src/main.rs` and e-mail addresses are not
mentions.

## Agent definitions

An agent is a Markdown file with optional YAML front matter. The body is the
agent's instructions.

```markdown
---
name: test-runner
description: Runs the test suite and reports failures with file and line.
tools: Read, Grep, Glob, Bash
mode: write
max_turns: 20
model: local:gguf:qwen3-coder
---
Run the narrowest relevant tests first, then the full suite. Report each
failure with the file, line and the assertion that failed.
```

| Field | Meaning |
| --- | --- |
| `name` | 1–80 letters, digits, `-` or `_`. Defaults to the file name. |
| `description` | Shown to the main agent when it chooses an agent, and in the `@` menu. |
| `tools` | Allowed tools, as a list or comma-separated text. Claude Code names map to ShadowCode tools: `Read`, `Write`, `Edit`, `MultiEdit`, `Glob`, `Grep`, `LS`, `Bash`, `WebFetch`, `WebSearch`, `TodoWrite`. Native names and `mcp__server__tool` names are used as written; `mcp__server` covers one server's tools. `Bash(git diff:*)` selects the whole tool. Leave it out to allow every tool the mode allows. |
| `disallowedTools` (or `deny`) | Tools to remove. |
| `tools` as a map | opencode's form, `{write: false, bash: false}`: `false` removes a tool. |
| `mode` | `read-only` (default) or `write`. Without `mode`, an agent whose `tools` include a writing tool (`Write`, `Edit`, `Bash`, …) is a write agent. opencode's `primary`/`subagent`/`all` values are ignored. |
| `max_turns` (or `maxTurns`, `steps`) | Step limit, 1–200. Never more than the parent's `agent.max_steps`. |
| `model` | A model id from the picker. Claude Code aliases such as `sonnet` or `inherit` are not ShadowCode ids; the parent's model is used and the run records a note. |

Other fields (for example `color` or `temperature`) are listed as ignored in
`GET /api/agents`. A definition can only narrow what a subagent may do. It
never changes the project's trust, permission mode, approvals or network
settings.

### Where definitions are read from

The first definition of a name wins; later ones are listed as shadowed:

1. `.shadow/agents/`
2. `.shadowcode/agents/`
3. `.claude/agents/`
4. `.opencode/agent/` and `.opencode/agents/`
5. `~/.config/shadowcode/profile/agents/` (your profile, for every project;
   see [rules and skills](RULES_AND_SKILLS.md))
6. `~/.config/shadowcode/agents/` (the older per-user folder)
7. `agents/` of profiles imported from Git
8. Built-ins: `explore`, `plan`, `review` (read-only) and `general` (write)

Profile definitions can be switched off in **Settings › Rules & skills**.

A project file named like a built-in replaces it. Files are limited to 64 KB,
and at most 128 are read.

## Project instructions from other tools

These files go into the system prompt, each labelled as project guidance that
grants no permissions. Your profile's `AGENTS.md` comes before them, labelled
as your instructions ([rules and skills](RULES_AND_SKILLS.md)):

- `AGENTS.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`
- `.cursorrules`, and `.cursor/rules/*.mdc` rules with `alwaysApply: true`
- `.shadow/instructions.md` and `.shadow/memory/project.md`

A file whose content repeats one already included (a common `CLAUDE.md` that
copies `AGENTS.md`) is included once. Each file is cut at 24 KB and all of
them together at 48 KB; files left out are named so the agent can read them.
Cursor rules with only a `description` are listed by path and description, so
the agent can read the ones that fit the task. `@path` imports inside
`CLAUDE.md` are not expanded.

**Nested instructions.** When the agent reads, lists or changes a file, it also
receives the `AGENTS.md` and `CLAUDE.md` files of that file's folders (below
the project root), and Cursor rules whose `globs` match the file. Each is
delivered once per task, attached to the tool result that touched the folder.
Nested files are cut at 8 KB, with at most 8 files and 24 KB per task.

## Claude Code commands and skills

- `.claude/commands/<name>.md` files are slash commands (`/name`), like
  `.shadow/commands/`. `$ARGUMENTS` is replaced with what you type after the
  command. `argument-hint` is shown in the `/` menu. Only files directly in
  the folder are read; Claude Code's `folder/name` namespaces are not.
- `.claude/skills/<name>/SKILL.md` files are skills, like `.shadow/skills/`.
- Claude Code fields that ShadowCode does not use (`allowed-tools`, `model`,
  `hooks`, `permission-mode`) are ignored in `.claude/` files. In ShadowCode's
  own folders they are still refused, so a constraint is never silently
  dropped there. Models and permissions come from ShadowCode's settings.
- A ShadowCode command or skill with the same name hides the `.claude/` one;
  the hidden file is listed as an issue in the Skills panel.

**Skills the agent loads itself.** The system prompt lists each skill's name
and description (at most 48 skills and about 6 KB). When a skill fits the task,
the agent calls `load_skill` to read its instructions (cut at 32 KB). A skill
with `disable-model-invocation: true` is not listed and cannot be loaded by
the agent; you can still run it with `/skill <name>`.

## MCP servers

- **First-class tools.** Tools of the MCP servers you enabled for the project
  are offered to ShadowCode's agent directly, named `mcp__<server>__<tool>`
  with the server's own input schema. Each call still asks for approval with
  the exact arguments, like `mcp_call`. The first task after a server is
  enabled starts it once to read its tool list; later tasks reuse that list
  and start the server only when a tool is called. When the enabled servers
  offer more than `mcp.inline_tools` tools (default 40, `0` turns this off),
  only `mcp_tools` and `mcp_call` are offered. Those two stay available in
  every case.
- **Vendor CLIs.** The same enabled servers are passed to vendor CLIs for each
  run: Cursor, Grok and Antigravity through ACP `session/new` and
  `session/load` (`mcpServers`; HTTP servers only when the agent reports
  `mcpCapabilities.http`), Claude Code through `--mcp-config`, and Codex
  through `-c mcp_servers.<name>.…` overrides. They are added to the vendor's
  own MCP settings for that run and are never written to the vendor's files.
  Servers that use stored secrets (`env_refs`, `api_key_env`) or literal `env`
  values are not shared, so credentials never appear on a command line. Plan
  and Review tasks and untrusted projects share none. Set
  `mcp.share_with_cli_agents: false` to stop sharing.

## Settings

`config.yaml`:

```yaml
subagents:
  enabled: true      # offer spawn_agent to ShadowCode's own agent
  max_parallel: 4    # subagents of one task running at once (1–8)
  max_depth: 1       # 1 = no grandchildren (0–3; 0 turns subagents off)
  max_turns: 24      # step limit when a definition sets none (1–200)
  max_per_task: 16   # subagents one task may start (1–64)
mcp:
  inline_tools: 40             # first-class MCP tool schemas (0–128)
  share_with_cli_agents: true  # pass enabled servers to vendor CLIs
```

## API and events

- `GET /api/agents?workspace=` → `{agents, shadowed, issues, dirs, settings, user_dir, workspace}`.
  Each agent has `name, description, model, tools, deny, mode, max_turns,
  source, path, hash, ignored, instructions_preview`.
- `GET /api/subagents?session_id=` → `{runs}` for one parent conversation,
  oldest first. `GET /api/subagents/{run_id}` → one run: `agent, description,
  prompt, mode, model, parent_session, job_id, session_id, status, summary,
  error, files, binary_files, patch, applied, usage, steps, notes`.
- `GET /api/sessions?include_subagents=true` includes subagent conversations;
  every row carries `subagent_parent`. `GET /api/sessions/{id}` includes
  `subagent_parent` and `subagent_run`.
- Events in the parent conversation: `subagent.started {run_id, agent,
  description, prompt, mode, model, job_id, session_id, depth}`,
  `subagent.finished {run_id, status, summary, error, files, patch, usage,
  steps, notes, duration_s, …}`, `subagent.applied {run_id, agent, paths}`,
  and `context.attached {path, origin: "nested_guidance"}` for nested
  instructions. `mcp.warning {server?, text}` reports an MCP tool list that
  could not be read or was too large to offer as first-class tools.
