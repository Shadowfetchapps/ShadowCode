# Native commands and project skills

> **Advanced.** This is reached through Settings › Advanced › Skills and the `/` command menu. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md).

The native desktop runs slash commands through the Rust command service. Type
`/` in the composer to see available commands. Command results appear in the
conversation and survive reload; model tasks stream through the normal task,
approval, cancellation, and checkpoint system.

## Start work

- `/plan <task>` starts a read-only planning task.
- `/review [focus]` starts a read-only review. Without a focus, it inspects the
  current project changes for supported findings.
- `/test` asks the Test model to inspect the project and run relevant checks.
- `/test <command>` and `/run <command>` run that exact user-supplied terminal
  command and report its actual exit status. These use the Terminal policy;
  shell side effects are not file-tool checkpoints.
- `/goal <instruction>` creates and runs a durable goal with milestones.
- `/skill <name> [context]` runs a project skill. A unique skill or custom command
  is also available directly as `/<name> [context]`.

The composer’s selected model is an explicit override. Otherwise the effective
Plan, Review, Test, or Build purpose uses the configured model routing. A workflow
cannot widen a Plan or Review task into a writing task. A workflow declaring
`mode: plan` or `mode: review` also restricts a Build invocation. Existing project
trust, workspace permissions, approval rules, and queue limits still apply.

[Project plugins](NATIVE_PLUGINS.md) can install skills and commands together.
Their names use `plugin--component`, and they run through this same explicit
invocation, routing and permission flow. Supporting text stays beside each skill.

## Create a skill

Open **Skills & instructions**. Project instructions in
`.shadow/instructions.md` guide every task. Skills are loaded into model context
only when explicitly selected; discovering a skill does not execute anything.
**Use /name** places the invocation in the composer so you can add context and
choose the task mode and model before sending it.

The editor saves `.shadow/skills/<name>.md`. It validates the definition before
writing, preserves front matter when editing, and rejects overwriting a file
that changed since it was loaded. Skills in other supported directories are
shown with their source path and instructions; edit those source files directly.

For example, `.agents/skills/review-change/SKILL.md`:

```markdown
---
name: review-change
description: Inspect a change and report concrete defects
alias: inspect-change
mode: review
---
Read the relevant source and pending diff for $ARGUMENTS.
Explain each finding with a file location and a concrete failure scenario.
If there are no supported findings, say so and identify verification limits.
```

Run `/skill review-change src/parser.rs` or `/review-change src/parser.rs`.
`$ARGUMENTS` and `{{args}}` are literal text substitutions, made once. Arguments
containing template tokens or shell syntax are not recursively expanded or
executed. If no placeholder exists, arguments are appended as the additional
user request. Supporting files remain in the workspace and can be read with
the ordinary file tools relative to the skill directory.

## Discovery and limits

The native catalog reads these project paths:

- `.shadow/commands/*.md`, with legacy `.yaml`/`.yml` files treated as plain
  prompt text.
- `.shadow/skills/*.md` and `.shadow/skills/<directory>/SKILL.md`.
- The same flat-file and `SKILL.md` forms under `.shadowcode/skills` and
  `.agents/skills`.
- Claude Code's `.claude/commands/*.md` (commands) and
  `.claude/skills/<directory>/SKILL.md` (skills). A ShadowCode definition with
  the same name hides the `.claude/` one and the hidden file is reported.

Names and aliases use 1–80 ASCII letters, digits, hyphens, or underscores.
Optional YAML front matter accepts text `name`, `description`, `alias`, and
`mode` fields; mode is `code`, `plan`, `review`, or `test`.
`user-invocable: false` makes a definition unavailable to this explicit-invocation
catalog.

Skills can also be loaded by the agent itself: the system prompt lists each
skill's name and description, and the agent reads a skill's instructions with
`load_skill` when it fits the task (at most 32 KB). `disable-model-invocation:
true` keeps a skill out of that list; it can still be run with `/skill <name>`.
`argument-hint` is shown next to a command in the `/` menu.

The native parser rejects unsupported operative fields such as `model`, `hooks`,
`allowed-tools`, and `permission-mode` in ShadowCode's own folders instead of
silently ignoring constraints. In `.claude/` files, which are written for
Claude Code, those fields are ignored. Configure models and permissions in
Settings. Malformed definitions appear as
discovery issues. Ambiguous names and aliases are rejected. Built-in names stay
reserved; a skill with the same name can be invoked using `/skill <name>`.
Discovery remains confined to the project and does not follow directory symlinks
to load external skills.

Your profile (`~/.config/shadowcode/profile/`: `skills/`, `commands/`, and
imported profiles) adds skills and commands to every project. A project
definition with the same name or alias wins, and the profile one is reported
as not used. Profile files may carry fields other agents use (`allowed-tools`,
`model`, …); they are ignored there. Each item can be switched off in
**Settings › Rules & skills**. See [rules and skills](RULES_AND_SKILLS.md).

With a vendor CLI model (Claude Code, Codex, Cursor, Grok, Antigravity),
`/name` and `/skill name` send the skill's expanded instructions as the
prompt, since the vendor does not know ShadowCode's commands.

Each definition is limited to 64 KB, arguments to 32 KB, and expanded workflow
text to 128 KB. Discovery accepts at most 256 files and 2 MB of file contents.
The selected source, hash, and effective mode are recorded with the task.
Instructions are frozen when the task is queued; later file edits affect future
invocations. Very large guidance can still exceed the selected model’s context
budget and produces an explicit error.

## Other commands

`/help` lists built-ins and valid project workflows. `/status`, `/models`,
`/model [id]`, and `/router [on|off]` inspect or update model configuration.
`/git` and `/diff [path]` inspect repository state. `/cost` and `/context` show
recorded conversation usage and the configured context limit; they do not infer
prices or bill local inference.

`/new` and `/clear` create a conversation without deleting history. `/branch
[title]`, `/resume <unique-id-prefix>`, and `/pin [label]` manage saved work.
`/checkpoints` lists file-tool checkpoints in the current conversation, `/undo`
restores the latest available checkpoint, and `/rollback <task-id>` restores a
specific listed checkpoint. Conflict checks preserve unrelated edits.

`/memory [note]` reads or appends `.shadow/memory/project.md`. Saved notes guide
future tasks; the append command limits the file to 16 KB so it fits the project
guidance excerpt. These are user-written project notes, not a generated summary
of hidden model reasoning.
`/memory --task <TASK_ID> [note]` targets notes for an existing task in this
project. `/memory` also shows the selected conversation's latest task notes.
See [native memory](NATIVE_MEMORY.md) for continuation, branches, exports and
replacement with stale-content protection.

`/background` opens process controls; `list`, `start <name> <command>`, and
`stop <id>` manage the current project’s processes. `/sessions`, `/skills`,
`/goals`, `/health`, and `/settings` open the corresponding controls. `/expand`
toggles the latest tool card. `/quit` and `/exit` close the native application
through managed cleanup.

See [native verification](archive/NATIVE_VERIFICATION.md) and the remaining
[release gates](archive/NATIVE_MIGRATION.md).
