# Goals in the native desktop

> **Advanced.** This is reached through the drawer's Tools › Goals. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md).

ShadowCode stores goals, milestones, their task IDs, and
run state in the same SQLite database as conversation history. Goals imported
from 0.19 retain their original checklist and completion state.

Open the drawer's **Tools › Goals** (`Ctrl+K`, *Goals and milestones*), describe the desired outcome, then use
**Plan** to save it or **Plan & run** to begin. New goals start with three visible
milestones: inspect the project and define acceptance checks, implement the
change, and run the checks. The inspection milestone uses read-only Plan mode.
This initial checklist is deterministic; it is not a model-generated estimate.

Each milestone runs as a normal native task with the selected model, workspace
permissions, approvals, token/step limits, durable events, and checkpoints. Tasks
share the workspace queue with ordinary work. The conversation follows newly
started milestones; **Open task** returns to the goal's saved conversation.
Expand **Result** under a milestone to inspect its saved summary.

When [model routing](NATIVE_ROUTING.md) is enabled, the default inspection uses
the Plan model, implementation uses Build, and required command verification uses
Test. Selection happens when each milestone enters the task queue and is recorded
in its conversation. Otherwise milestones use the configured default model.

## Completion and evidence

The default inspection milestone must actually read or search the workspace.
The final verification milestone requires a successful recorded terminal command.
A missing verification command, a last failed command, or a failed/cancelled task
cannot automatically mark that verification milestone complete. The runner stops
at the first failure and displays its reason. A successful command is evidence
of that command's result; it does not establish that the chosen checks cover
every requirement. Review the transcript and changes for that judgment.

The checklist can also be updated manually while stopped. The tick control is
explicitly labelled **Mark complete manually**, and the saved result records
that action. Manual completion is distinct from model execution evidence.

## Pause, recovery, and bounds

**Pause** cancels the active model request or managed command and waits for its
cleanup. Completed milestones remain complete; the interrupted milestone becomes
pending. **Resume** skips completed work and continues in the same conversation.
If that conversation was deleted while stopped, resuming creates a new one while
retaining the goal's saved checklist and results.
Stopping a milestone from the main composer also pauses its goal.

Closing the app pauses its active goals after task cleanup. After an unclean
exit, interrupted goals remain paused for review; commands are never replayed
automatically. **Abandon** retains the checklist and history. Deleting a stopped
goal removes its checklist, while the underlying conversation remains available.
Running goals prevent deletion of their conversation and manual checklist edits.

The engine permits up to 16 running goals, 32 milestones per goal, and the shared
limit of 64 running/queued tasks across four active workspaces. SQLite schema 22
adds run state and milestone mode/verification fields, with a consistent backup
before migration. Legacy goal import names its columns explicitly so old files
remain compatible and unchanged.

Native command clients may supply a `milestones` array to `POST /api/goals`, with
`title`, `mode` (`code`, `plan`, or `review`), and `require_verification` fields.
Command verification requires `code` mode. The desktop creates the default
checklist. Run, pause, abandon, delete, and manual milestone changes use the
shared application service, so future CLI clients use the same lifecycle.

See [verification evidence](archive/NATIVE_VERIFICATION.md) for tests and
[migration gates](archive/NATIVE_MIGRATION.md) for the remaining release requirements.
