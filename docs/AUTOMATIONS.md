# Automations, issues and the GitHub Action

> **Guide.** Three ways to start work without typing it each time: prompts
> that run on a schedule, a task started from a GitHub or GitLab issue, and
> ShadowCode running in GitHub Actions. For everyday use see the
> [user guide](USER_GUIDE.md); routes are in the
> [API contract](API_CONTRACT.md#automations).

## Scheduled automations

An automation is a saved prompt with a schedule. Each time it runs, it is a
normal task in a new conversation named "_name_ · automation", so you can
open it, read what it did and continue from it.

Open **Tools › Automations** in the drawer (or _Automations_ in `Ctrl+K`) and
choose **New automation**:

- **Name** and **What should it do?** — the prompt, exactly as you would type
  it in the composer.
- **Model** — any ready model from the picker, or _The project's model_
  (the one last chosen in the composer for this project).
- **Mode** — _Code_ may edit files; _Plan_ and _Ask_ are read-only.
- **When** — every hour (at a minute), every day, weekdays (Monday to
  Friday), every week (a day), or a custom five-field cron expression
  (`minute hour day month weekday`, with ranges `1-5`, lists `1,3`, steps
  `*/15`, names `MON`/`JAN`, and `@hourly`, `@daily`, `@weekly`,
  `@monthly`). Times are this computer's time or UTC. The editor shows the
  next three times as you type.
- **How it runs**
  - _In a fresh worktree_ (the default in a Git repository): the run gets its
    own managed checkout at the project's current commit, so your working
    folder is never touched. When the run changed nothing, the worktree is
    removed afterwards and the conversation points back at the project. When
    it changed files, the worktree and its `shadowcode/<id>` branch are kept:
    review or bring the changes back from **Tools › Worktrees**. It is also
    kept while another task (a follow-up you sent in the run's conversation)
    or a background process is using it.
  - _In the project folder_: the run waits behind any task already running
    there.
  - _Read-only, even in Code mode_. A run never gets more permission than the
    project has; this lowers it further.
  - _When it asks for approval_ — by default the run **stops** (status
    "Stopped for approval"), because nobody may be watching. Tick _wait for
    me_ to leave the request open in the conversation until the time limit.
  - _Stop after N minutes_ — the time limit (1–1440, default 60), including
    time spent waiting in the queue.
  - _Catch up missed runs within N minutes_ (default 120; 0 never catches
    up), see below.
  - _Notify me when it finishes_ — a desktop notification with the
    automation's name when the run finishes or stops, instead of the usual
    task notification (the notification settings still apply). A request for
    approval notifies like any other.

Each automation shows its schedule, the next time and the last result, with
**Run now**, **Pause** / **Resume**, **History**, **Edit** and **Delete**
(its conversations stay). **History** lists every run with its status, when
it ran, why (on schedule, caught up, run now), how long it took and its cost
or tokens, with a link to the conversation.

### When automations run

Schedules run while ShadowCode is open: the desktop window, or a headless
`shadowcode serve` (for example as a login service). One-shot commands such
as `shadowcode run` never start schedules. The list says so when nothing is
running them.

- **Only one run at a time.** If an automation is still running when its next
  time comes, that time is recorded as _Skipped_; **Run now** says it is
  already running.
- **Missed times.** When ShadowCode starts (or the computer wakes) after a
  scheduled time, the automation runs once if that time is within its
  catch-up window, and the history says _Caught up after start_. Older times
  are recorded once as _Missed_ (with how many times were missed) and the
  automation waits for its next time. Paused time is never caught up:
  **Resume** starts from the next time after now.
- **Daylight saving.** In this computer's time, a time skipped when clocks go
  forward runs at the first minute after the change, and a time that happens
  twice when clocks go back runs once.
- **Interrupted runs.** A run still going when ShadowCode quits is stopped
  with it and shows as _Interrupted_ next time.

- **Spending limits.** A run on a paid API model that reaches its
  [spending limit](USER_GUIDE.md#spending-limits-for-paid-models) stops
  (status _Stopped at the spending limit_), or waits for your answer when the
  automation waits for approvals.
- **Resume after a plan limit.** "Resume at 3:40 PM" on a conversation that
  hit a subscription's plan limit is a one-shot continuation that the same
  scheduler starts at that time; it needs ShadowCode open then, like any
  schedule.

Statuses: Running, Finished, Failed, Stopped (by you), Hit its time limit,
Stopped for approval, Stopped at the spending limit, Interrupted, Missed,
Skipped.

## Start from an issue

**Tools › Issues** (or _Start from an issue_ in `Ctrl+K`) lists the open
issues of the project's GitHub or GitLab repository using the GitHub CLI
(`gh`) or GitLab CLI (`glab`) with your own sign-in. If the CLI is missing
or signed out, the panel says how to install it or which sign-in command to
run, with a button to open the Terminal.

Pick an issue to see its text, then **Start task from #n**:

1. It suggests a branch `issue-<n>-<short-title>` and, unless you untick
   _Work on a new branch_, creates and switches to it (or switches to it if
   it already exists).
2. The message box is filled with the issue's title, link, description and
   its five newest comments, each shortened, quoted as the reporter's words
   and marked as a description rather than instructions. Read and edit it,
   then send it with the model you choose.
3. When that task finishes, a banner offers **Open PR that closes #n**: it
   opens the Git tab with the title filled in and `Closes #n` in the
   description, and the usual **Create pull request** flow pushes the branch
   and opens it.

## GitHub Action

[`integrations/github-action`](../integrations/github-action/README.md) runs
ShadowCode in GitHub Actions: a pinned release is downloaded and checked
against its `SHA256SUMS`, `shadowcode run --json` runs headless with an
OpenRouter (or other OpenAI-compatible) key from repository secrets, and the
result becomes a pull request or a comment made with the workflow's
`GITHUB_TOKEN`. Two examples are included: `/shadowcode <task>` in an issue
comment (trusted commenters only) and a label that asks for a read-only
pull request review. The action's README covers inputs, outputs and the
security rules (trusted commenters, never `pull_request_target` with
untrusted code, no persisted checkout credentials, least-privilege
permissions).

`shadowcode run --approval approve` (used by the action's `approval:
approve`) grants the task's own approval requests. It is meant for machines
that are thrown away after the job, like GitHub-hosted runners.
