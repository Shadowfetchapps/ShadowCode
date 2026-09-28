# Background processes in the native desktop

> **Advanced.** This is reached through the drawer's Tools › Processes. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md).

Open the drawer's **Tools › Processes** (`Ctrl+K`, *Background processes*).
Give the process a name, enter its command, and choose **Start process**. This is
also available through `/background start <name> <command>`; `/background list`
and `/background stop <id>` inspect and stop a process. It is
useful for a development server or watcher that should continue while you work
on other tasks in the same project. Commands run in the selected project folder.

The panel shows process status, PID, start time, exit code, and live output.
**Stop** waits for cleanup and retains the cancelled result. Stopping an already
finished entry preserves its original completed or failed status. Duplicate
active names are rejected within the same project.

## Output and history

The runner drains stdout and stderr continuously, including output without a
newline. It preserves UTF-8 characters split across reads and keeps the most
recent 64 KB of combined output in memory and SQLite. When that limit is reached,
the panel indicates that older output was omitted. This is a log tail, not a
complete terminal recording or interactive shell; stdin is closed.

The process list sends a 4 KB preview per entry so a large history does not
repeatedly transfer all retained logs. **Read retained output** opens a snapshot
of the stored tail and full command; **Refresh retained output** updates that
snapshot. **Return to live output** returns to the automatically refreshed preview.

The panel shows the latest 100 entries for the current project, plus every active
process even if it started before that history window. Older entries remain in
SQLite. History and the retained log survive application restarts. The native
migration imports legacy `background.db` history once without changing the
original database; legacy active entries become interrupted history.

## Permissions and lifecycle

Starting from the panel or CLI is an explicit user command, like the Terminal
Run button. The project must be trusted, read-only mode denies execution, and
configured command restrictions still apply. The manager permits up to four active
processes per project and sixteen per application profile. Background commands
deliberately run alongside coding tasks; their shell side effects are not file
tool checkpoints and are not undone by task rewind.

Processes continue between tasks and while you navigate to another project.
Return to their original project to view or stop them. They do not inherit the
foreground terminal timeout. On Stop or normal application shutdown, ShadowCode
sends SIGTERM, allows up to two seconds for the parent process to exit, and then
kills remaining members of the managed process group. Shutdown waits for output
draining and persisted status. A subprocess that deliberately detaches into a
different session is outside that process group; this runner is not an OS sandbox.

After an abrupt crash or forced termination, previously active records are marked
**INTERRUPTED**. ShadowCode does not restart commands automatically or signal a
persisted PID, which could now belong to an unrelated process. An interrupted
record does not prove that an independently detached process has exited.

## Asking a model to manage a server

Build tasks expose `background_start`, `background_list`, `background_output`,
and `background_stop`. Ask the model to start a named server or watcher, inspect
its logs, and verify that it responds. Registration or a live PID alone does not
prove readiness. The process appears in the same Background panel and CLI list
as a manually started process.

`background_start` follows the task's shell permissions, including command
restrictions and shell approval settings. Its approval shows the actual command,
project and name, and explains its independent lifetime. Enabled `before_command`
hooks can block startup. Starting or stopping a process invalidates the task's
previous file observations; subsequent edits still require current hashes.

These are **project processes**: once started, they continue after the coding
task completes, fails or is cancelled. Use the panel, CLI, or `background_stop`
to end them; closing the owning application also stops them. Cancelling an
unanswered start approval does not start anything. Process history retains the
originating task ID even if that conversation is later deleted.

`background_stop` requires a scoped approval showing the recorded process name,
exact ID, project and command, and waits for managed cleanup. A model must use
the ID returned by a background tool, never a PID. Every lookup and control is
restricted to the task's current project. Plan and Review tasks expose only
list/output, and cannot start or stop processes.

Model listings include every active process and up to twelve recent finished
entries, with 512-byte log previews. Output reads default to an 8 KB tail and
accept `max_bytes` up to 64 KB. Responses identify omitted output/history and
clip command previews to 1 KB; approval prompts retain the complete command.

The desktop uses Rust IPC routes `GET/POST /api/background`,
`GET /api/background/<id>`, and `POST /api/background/<id>/stop`.
The [native CLI](NATIVE_CLI.md) exposes `background start`, `list`, `logs`, and
`stop`; starting requires an open desktop or persistent `shadowcode serve`
owner. Both share the same manager used by model tools.

See [verification](archive/NATIVE_VERIFICATION.md) and the remaining
[release gates](archive/NATIVE_MIGRATION.md).
