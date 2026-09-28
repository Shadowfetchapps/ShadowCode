# Compare

Open **More › Compare** in the composer to send one task to 2 or 3 models.
Each model works in its own
lane, which is a managed Git worktree on a `shadowcode/<ID>` branch. When the
lanes finish, you keep one result. Its changes are applied to your project's
working tree, and every lane worktree and its branch is removed.

## Starting state

In 0.33.1, Compare refuses to start while app-owned editor drafts differ from
their saved base. Save or discard the listed drafts first. The native snapshot
also checks persisted recovery drafts and coordinates with short workspace
writes from cooperating ShadowCode processes. Other editors' unsaved buffers
and arbitrary terminal commands are not covered by that cooperative lock.

Every lane starts from the same commit: HEAD plus the work you have not
committed yet. That covers staged and unstaged changes to tracked files, and
untracked files that `.gitignore` does not exclude. Ignored files, such as
build output and `.env`, are not copied. When you have uncommitted work,
ShadowCode records it as a "ShadowCode compare base" commit on top of HEAD and
starts each lane there. That commit is only reachable from the lane branches.
The record says whether uncommitted work was included (`base.included_uncommitted`).

Your checkout is not modified. ShadowCode captures the working tree with a
temporary index file and writes only new Git objects. It never resets,
stashes, checks out or commits in your checkout, and it does not touch your
index.

The project must be a trusted Git repository root with at least one commit
and no unresolved merge conflicts. Submodule contents are not checked out in
lanes.

## Lanes

Each lane is a normal ShadowCode job with its own conversation. The job uses
the chosen model, the same permission mode and permission level as the
project, and the chosen mode: `code`, `plan` (read-only) or `ask` (read-only
review). Cloud lanes can run concurrently; managed local lanes run sequentially. While lanes exist, their worktrees are
trusted for you. Keep and discard remove that trust again.

- **Local models run sequentially.** Choose two or three installed compatible GGUF models. Managed jobs share one runtime and run in submission order; queued lanes remain cancellable. Comparison runs refuse automatic GPU-to-CPU fallback, including reuse of a runtime that automatically fell back. Choose CPU settings deliberately or retry the GPU configuration. Runtime preparation time includes readiness/wait work and is not token-generation latency.

- **Offline mode** accepts only models that run on this computer.
- **Cost.** Every lane is a full task. Subscription lanes use your plan's
  allowance, OpenRouter and other API-key lanes are billed per token, and a
  3-lane comparison costs about three times as much as a single task.

By default, lane conversations are left out of `GET /api/sessions`. Add
`include_compare=true` to include them. Every session row, and
`GET /api/sessions/<id>`, has `compare_id` and `compare_lane` (the lane's
model id) fields, which are null for ordinary conversations. A follow-up turn
in a lane conversation runs in that lane and updates the lane's status and
changes.

## Keep, discard, cancel

- **Keep** works only after that lane has finished. A lane that failed or was
  cancelled can still be kept if it made changes. ShadowCode commits the
  lane's result on its managed branch. It then checks the diff against the
  lane's base with `git apply --check` and applies it to your working tree
  only: nothing is staged or committed, and you review the result in the
  Changes view. If a lane is still running, ShadowCode stops it first. The
  scoreboard is updated, then every lane worktree and its `shadowcode/…`
  branch is removed. No other branches are touched.
- If your project changed after the comparison started and the result no
  longer applies cleanly, Keep refuses and names the conflicting files. Nothing
  is changed and all lanes stay in place, so you can resolve the conflict and
  keep again.
- **Discard** stops running lanes and removes all lane worktrees and their
  branches.
- **Cancel** stops running lanes but keeps their worktrees. The comparison
  becomes `done` once they stop.
- If a lane worktree is deleted outside ShadowCode, that lane cannot be kept.
  Keep and Discard still work for the other lanes. Git's registration and
  branch for the missing lane are cleaned up, and a note is added to the
  record.

Lane worktrees count toward the limit of 64 managed worktrees. Keep or
discard finished comparisons to free them.

## Scoreboard

For each project, ShadowCode records how many finished comparisons each model
took part in (`runs`) and how many of them it won (`wins`). A comparison
counts as finished once all of its lanes finish, or when you keep a result. A
comparison you discard before it finishes does not count.

## API

| Request | Result |
| --- | --- |
| `POST /api/compare` `{workspace?, task, models: [2–3 distinct picker ids], mode?: "code"\|"plan"\|"ask", web?: bool}` | The comparison record |
| `GET /api/compare/<id>` | The record, updated with the lanes' current status |
| `GET /api/compares?workspace=<path>` | `{compares: [record…]}`, newest first (up to 20) |
| `POST /api/compare/<id>/keep` `{model}` | The record, with `state: "applied"` |
| `POST /api/compare/<id>/discard` | The record, with `state: "discarded"` |
| `POST /api/compare/<id>/cancel` | The record |
| `GET /api/compare/scoreboard?workspace=<path>` | `{workspace, rows: [{model, name, wins, runs}]}` |

`workspace` defaults to the selected project.

Record:

```text
{id, workspace, task, mode, web, created_at, finished_at,
 state: "running" | "done" | "applied" | "discarded",
 base: {commit, head, included_uncommitted},
 lanes: [{model, name, session_id, job_id, worktree, worktree_id, branch,
          base_commit, status, summary,
          changed_files: [{path, status, additions, deletions, binary}],
          changed_files_truncated,
          checks: {passed, failed, commands: [{command, exit_code, success}]},
          duration_s, usage, error, removed}],
 winner, applied_files, notes}
```

A lane's `status` is its latest job's status: `queued`, `running`, `paused`,
`cancelling`, `completed`, `failed`, `cancelled`, `limit_reached` or
`interrupted` (the app stopped while the lane was running). `changed_files`
compares the lane worktree with its base, including untracked files. `checks`
comes from the job's verification summary. `notes` lists cleanup problems and
lanes that went missing.

Records are stored in the profile database (`native_meta` keys
`compare:<id>`, `compare_index:<workspace>` and
`compare_scoreboard:<workspace>`), so they persist across restarts.
