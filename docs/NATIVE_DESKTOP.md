# Native desktop

> **Reference.** This covers the desktop build and packaging. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md).

The `main` branch builds one Rust desktop executable with the React
interface embedded. Tauri hosts it in the system WebKit webview. The interface
calls the Rust engine through IPC; desktop operation needs no HTTP listener,
Python interpreter, Node runtime, or browser launcher.

The archived [release gates](archive/NATIVE_MIGRATION.md) record the
acceptance checks of earlier packages; current release steps are in
[RELEASING.md](RELEASING.md).

## Attaching to a running engine

Open the desktop with the same profile as a running `shadowcode serve` or TUI.
The window attaches over the private Unix socket instead of opening another
SQLite manager. Its project and conversation selection remain independent of
other clients. Global settings, approvals, task history and project queues are
shared. A banner explains that closing this attached window leaves engine-owned
tasks and background processes running. Stop them explicitly, or stop the
owning server/TUI. A desktop that starts the engine itself still stops managed
work when it closes.

Each attached view holds one private connection; ordinary requests use separate
connections so a slow command cannot block status reads or cancellation. At most
four views may attach. Closing the view, losing its connection, or stopping the
owner releases its navigation state. Views do not persist project selection to
the owner's remembered-workspace file. A temporary foreground CLI command is
not a persistent host and rejects desktop attachment. Version mismatches are
rejected before attachment.

Attached windows receive event hints through their private lease connection;
there is no window timer repeatedly waking an idle engine. The event reader
still fetches committed rows and polls as a fallback. Each hint reaches the
window as `shadowcode:events {session_id, type}`; pending approvals and the
job list are read (`GET /api/feed`) only for hints that can change them, with
a 15 s backstop read. Approval, failure, plan-limit and completion hints use
the same native notification path as an engine-owning desktop, respecting the
profile's notification settings and suppressing notifications while focused on
that conversation.
Hints never contain model-stream bodies or complete tool output. Their frame
limit is 16 KiB, the local broadcast buffer holds 64 hints, and completion
summaries contain at most 180 Unicode characters. A lagging receiver requests
durable replay; a blocked socket releases its view after five seconds.

If the owner exits or restarts, the attached window waits for a matching
engine on the same socket and reopens its view lease. Reattach does not
start jobs, retry POST/mutating commands, or replay completed tools.
Existing subscribers receive `view.reattached` on the same wakeup channel
and then fetch committed rows from the last cursor. Closing a disconnected
window still succeeds if the new engine never returns.

## Build and run

On Ubuntu 24.04, install the native build dependencies:

```sh
sudo apt-get install build-essential pkg-config libgtk-3-dev \
  libwebkit2gtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev patchelf
```

Use Rust 1.95 and Node 22.12+ or 24+ for development. From the repository root:

```sh
npm --prefix ui ci
npm --prefix ui run build
cargo build -p shadowcode-desktop --locked
./target/debug/shadowcode --version
./target/debug/shadowcode --profile /tmp/shadowcode-dev --workspace /path/to/project
```

`--profile` keeps config, secrets, SQLite history, and webview storage separate
from the installed application. Without it, the application uses the existing
`shadow-agent` XDG directories and backs up SQLite before migrating. Do not run
the legacy Python application against the same profile during migration.

Native startup restricts its config, data and state directories to the current
account (mode 700), retaining existing files and parent-directory permissions.
Those three application directories must be real directories owned by your
account. To relocate storage through a symlink, link the XDG base or explicit
profile parent instead. An unsafe `native.lock` (symlink, extra hard link,
foreign owner or special file) causes a startup error without replacing it.

Pick a subscription, a local GGUF model or an OpenRouter model (after adding
a key, see [OpenRouter](OPENROUTER.md)) in the composer picker. Local models
need the managed llama.cpp runtime (`scripts/build-llama.cpp.sh` for a
development checkout; packages bundle it). See [local models](LOCAL_MODELS.md).

The [native CLI](NATIVE_CLI.md) uses this same executable. Commands such as
`run`, `sessions`, and `health` run without a display and share an open desktop's
engine without changing its selected project. `serve` explicitly hosts the
engine headlessly for detached tasks and background servers.

[Project inspection and diagnostics](NATIVE_INSPECTION.md) are available as
`/understand`, `/understand --save`, `/doctor`, `/doctor --test-model` and
`/why [path]` in the conversation. Health shows the same native checks with
separate warning, failure and not-checked labels and readable repair suggestions.
Model connectivity is tested only when explicitly requested. Automatic native
diagnostic repairs remain a migration requirement.

[Project and task notes](NATIVE_MEMORY.md) persist through native commands and
the CLI/MCP server, with bounded continuation context, independent branch copies
and note exports. Approval decisions carry the displayed prompt's conversation
ID so a navigation change cannot send the decision to another task.

[Native SQLite inspection](NATIVE_SQLITE.md) gives models read-only table/query
tools, also available through the CLI and MCP server, with parameter binding,
live WAL support, cancellation and result limits.

[Queued follow-ups](NATIVE_QUEUE.md) let you send the next instruction while the
current task streams. The project queue shows waiting messages across
conversations, retains their model/mode choices and supports cancellation.
Reloads return to the running task before waiting follow-ups.

[Background tools](NATIVE_BACKGROUND.md#asking-a-model-to-manage-a-server) let
models start and inspect project servers under shell permissions and stop them
with explicit approval. The same processes appear in the Background panel;
they continue across coding tasks until stopped or the owning application closes.
Plan and Review tasks can inspect them without process control.

Project [lifecycle commands](NATIVE_HOOKS.md) can be reviewed and enabled in
Settings › Advanced › Hooks or through the same native CLI. Their command gates and
completion checks run inside the native task lifecycle without Python callbacks.

[Native MCP stdio and HTTP servers](NATIVE_MCP.md) can be registered and enabled in
Settings › Advanced › MCP. The agent discovers their tools lazily and requests approval for
each exact call. HTTP bearer secrets resolve only on connection, and remote
hosts require network access. Connections close before the task finishes.
`shadowcode mcp serve` also exposes the selected project to another client over
stdio, with read-only defaults and explicitly delegated write/approval access.
The native executable also serves authenticated loopback HTTP with a fixed
project and a gateway owner that survives individual client reconnects. The
[server guide](NATIVE_MCP.md#connect-another-coding-tool-to-shadowcode) lists
the implemented tools and remaining migration requirements.

To work on the live interface, run `./ui/node_modules/.bin/tauri dev` from the
repository root. The Tauri configuration starts Vite and builds the native app.
Build hooks explicitly use the `ui` directory, including when the CLI is invoked
from the repository root.

## Development packages

Packaging also requires a working Docker engine (or Podman with
`SHADOW_CONTAINER_ENGINE=podman`) for the pinned AppImage runtime build.
From the repository root, build both package formats with:

```sh
node scripts/build-native.mjs
node scripts/check-native-package.mjs \
  target/release/bundle/appimage/ShadowCode_VERSION_amd64.AppImage \
  target/release/bundle/deb/ShadowCode_VERSION_amd64.deb
node scripts/test-native-runtime.mjs
node --test scripts/test-native-source-fetch.mjs
node scripts/test-native-runtime-write-errors.mjs
node scripts/test-native-runtime-sources.mjs
```

The build script collects [dependency notices](../licenses/native/README.md),
restores the original executable before packaging each format because Tauri
modifies its bundle-type marker, and repacks the AppImage with notices for its
actual bundled system libraries. The [source-built runtime](../packaging/native-runtime/README.md)
uses private extraction directories and forwards ordinary shutdown signals. Its
recipe, notices, and source archive are verified with the package. Build tools
are cached in `target/.tauri/` and `target/native-runtime/`.
Unknown dependencies or a changed AppImage runtime stop packaging until their
notices are supplied. The checker verifies FUSE-free
startup, the legacy `shadow ui` launch form, native ELF code, package versions,
dependency resolution on the build host, and absence of Python interpreters,
libraries, and sidecars. It also verifies the SHA-256 digest of every listed
notice in each extracted package. It produces checksums in `artifacts/native-package/`.

The published packages are built and inspected on Ubuntu 24.04. The AppImage
includes the native GTK/WebKit runtime; the Debian package uses the system
libraries. Both packages are checked for embedded Python/Node runtimes and
include dependency notices and matching SHA-256 checksums.
Concurrent extraction-mode launches use independent private temporary directories.
One CLI exit or secondary activation cannot remove a running window's files.
The runtime regression checks concurrent owners, deferred resources, signals,
cleanup, `nohup`, and long paths. The packaged window test also checks repeated
default-profile activation using isolated XDG storage and a private DBus session.

`scripts/build-native.mjs` constructs a deterministic packaging PATH before
spawning cargo, Tauri, or linuxdeploy. It keeps `/usr/bin`, `/bin`, `/usr/sbin`,
`/sbin`, the Node directory that launched the script, the selected `CARGO_HOME/bin`
(default `~/.cargo/bin`) for Rustup, rust-dev extract bins when present, and
`target/{release,debug,.tauri}`. It drops `/usr/local/bin`,
`/snap/bin`, and any directory whose `node`/`npm` resolves into Hermes or
`/root`. The calling shell does not need to sanitize PATH; a dirty host PATH
that includes `/usr/local/bin/node` → `/root/.hermes/...` is ignored. The same
helper is used by `scripts/native-runtime.mjs` and `scripts/build-linux.sh`.
Prove it with `node --test scripts/test-native-packaging-env.mjs`.

## Desktop integration

- Native folder selection and conversation export use OS dialogs. External
  HTTP/HTTPS links open through the OS; remote pages cannot replace the app view.
- Default-profile launches focus the existing instance. Isolated profiles have
  separate webview storage and still enforce their own profile locks.
- Window geometry is retained for the default profile. Desktop notifications
  cover approvals waiting (and a warning 2 minutes before a vendor approval is
  denied), failed tasks, plan limits and finished tasks, each switchable in
  Settings › Appearance with an optional sound. They appear while the window
  is not focused or the task's conversation is not the one on screen; on
  Linux, clicking one focuses the window and opens that conversation.
- Close requests and termination signals cancel agent and manual terminal work,
  wait for cleanup, and retain a visible error if shutdown needs attention.
- Live events are wakeups for ordered SQLite replay. A periodic read recovers
  missed notifications; streamed messages are replaced by their final text.
- [Goals](NATIVE_GOALS.md) run persisted milestones through the same task engine,
  with visible results, command verification, pause/resume, and safe recovery.
- [Model routing](NATIVE_ROUTING.md) selects a registered model for Plan, Build,
  Review, and Test. Provider-scoped IDs keep identical model names on different
  servers distinct; conversations retain the actual selection and fallback notices.
- [Commands and project skills](NATIVE_WORKFLOWS.md) run selected workflows through
  the native task engine and retain command results and source provenance.
- [Background processes](NATIVE_BACKGROUND.md) run project servers and watchers
  alongside coding tasks, with live bounded logs, retained history, stop controls,
  and process-group cleanup during application shutdown.
- A private Unix socket connects native CLI clients to the desktop's engine.
  Each client keeps independent navigation state; managed shutdown closes the
  connection and cancels its in-flight manual commands before engine cleanup.

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm --prefix ui test
node scripts/test-native-cli.mjs
```

The actual window test uses Tauri's WebDriver bridge and a scripted compatible
model, with a disposable profile and project. Install `webkit2gtk-driver`, `xvfb`,
and `tauri-driver` 2.0.6, then run:

```sh
cargo install tauri-driver --version 2.0.6 --locked
node scripts/run-native-x11.mjs node scripts/test-native-desktop.mjs
node scripts/run-native-x11.mjs node scripts/test-native-markdown.mjs
```

The wrapper declares X11 before creating the private D-Bus session and uses a
fresh runtime directory with mode 0700. HOME and the original XDG config/data/state/cache
roots remain available for the existing read-only provider probes. Runtime
service discovery therefore differs from the physical desktop. No GTK portal
backend is mandated; this qualifies the controlled X11 app and Markdown/CSP
behavior, not physical Wayland or all portal features.

Both probes capture their direct private session identity and clean only newly
activated services bound to that exact bus, with PID/start/executable/user
revalidation. Cleanup is bounded and fails closed. The outer wrapper then
requires command/output closure, disappearance of recorded session processes,
and no surviving late activation before removing its runtime directory. It
never performs a general process kill. Incomplete output, unknown ownership,
a residual process or missing cleanup report fails the gate. Identity checks
are not atomic pidfd protection against a hostile PID-reuse race.

Session diagnostics are retained under `artifacts/native-x11/run-*` (override
`SHADOW_X11_ARTIFACTS`). The generated private-bus fixture exercises lifecycle
checks without opening a display:

```sh
node --test scripts/test-native-test-session.mjs
```

Optional environment variables `SHADOW_DESKTOP_BINARY`, `SHADOW_TAURI_DRIVER`,
and `SHADOW_WEBKIT_DRIVER` select explicit executable paths. Logs, screenshots,
and a machine-readable result are written to `artifacts/native/`. This test
checks the real embedded window, Rust IPC, approval, file and terminal tools,
reload, cancellation, compact layout, model routing/fallback notices, goals and
their live transcript, pause, manual and model-managed background processes,
CLI coexistence and project isolation,
project inspection and diagnostic status presentation,
light/dark/compact/goals/routing/background/skills/hooks/MCP/inspection/diagnostics accessibility,
background start/stop approval and shared panel state,
and managed shutdown. It also verifies
that the executable embeds the current compiled interface and does not load
`libpython`.

To run this same workflow against the actual AppImage, set
`SHADOW_DESKTOP_BINARY` to its absolute path,
`SHADOW_DESKTOP_ARGS='["--appimage-extract-and-run","ui"]'`, and
`SHADOW_NATIVE_ARTIFACTS` to a separate output directory. Its test profile and
workspace remain disposable, so it does not migrate the installed app's data.

See [verification evidence](archive/NATIVE_VERIFICATION.md) for engine stress tests and
separate real-model coding probes. The scripted window test establishes UI and
engine integration; the real-model probes establish provider/tool behavior.

To exercise an installed Ollama model through the actual native window, run:

```sh
xvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- \
  node scripts/probe-native-model.mjs gpt-oss:20b
# Repeat with another installed model:
xvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- \
  node scripts/probe-native-model.mjs qwen3:14b
```

This manual probe defaults to `target/release/shadowcode` and accepts the same
binary, argument, driver, and artifact environment variables as the scripted
window test. It uses a temporary Rust project and isolated profile, approves
only `cargo test --offline --lib` in that project, checks the minimal edit and
unchanged tests independently, reloads the conversation, submits a read-only
follow-up, clicks Stop during actual model streaming, restores the checkpoint,
and verifies native-process shutdown. Results and screenshots are saved under
`artifacts/native-model/<model>/`. Ollama must already have the selected model;
the probe does not download models or change the installed app's profile.

For the packaged single-instance regression, set
`SHADOW_NATIVE_DEFAULT_PROFILE=1` when running the window test under
`dbus-run-session`. The harness redirects XDG settings/history to its disposable
profile, then activates the existing window three times and reloads it before
continuing the full workflow. Run this mode only on a private test DBus session.
