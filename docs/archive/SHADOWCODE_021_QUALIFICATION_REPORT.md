# ShadowCode 0.21 qualification report

**Decision: do not auto-release. Keep version 0.20.0.**
The 0.21 architecture survived real process death, persistence, replay
policy, a full 1M SQLite insert, a real local-model harness, and
packaging on this host. That is not a license to bump or merge.

| Field | Value |
| --- | --- |
| Date | 2026-09-21 |
| Repo | `~/Documents/Codex/2026-09-19/new-chat/work/ShadowCode` |
| GitHub | https://github.com/ShadowfetchLinux/ShadowCode.git |
| Start branch | `feature/shadowcode-0.21-autonomy` |
| Start SHA (verified) | `5d33133bf7d61375aafdb44d6b51ef01a0d5cbe0` |
| Qualification branch | `release/shadowcode-0.21-qualification` |
| Main | not checked out, not merged, not force-pushed |
| Autonomy branch | left in place at `5d33133` |
| Version after this pass | **0.20.0** everywhere |

## What the previous phase did not execute

The autonomy journal recorded in-process reattach, a **60s / 139305-event**
debug SQLite probe, fixture provider chaos, and no AppImage/deb. Missing
from that pass and executed here where practical:

- Real OS-process engine/desktop attach, death, and reconnect
- Full 10k / 100k / 1M SQLite insert with no time cutoff
- Disposable-repo, self-host worktree, and git-destruction trials
- Real ollama harness (`gpt-oss:20b`)
- Hostile MCP definition against a live doctor
- AppImage + Debian build, inspect, extract-mode smoke
- Installer checksum present / mismatch / absent on an isolated `$HOME`
- Clippy/fmt via rust-dev extract, UI prod + tsc, Playwright e2e
- Longest-practical leak monitor (not 6 hours)

## P1 Baseline

Inspected journal, `ARCHITECTURE.md`, `SECURITY.md`, `NATIVE_DESKTOP.md`,
`RELEASING.md`, `NATIVE_VERIFICATION.md`, `NATIVE_BACKGROUND.md`,
`NATIVE_CLI.md`, `NATIVE_INSPECTION.md`.

Created `release/shadowcode-0.21-qualification` from `5d33133`.

First clean baseline (before most new tests):

- `cargo test --workspace --offline --lib --tests --bins`: **271 passed**
- `npm --prefix ui test`: **38 passed** (8 files)

After qualification tests and keep-list work, see P28 for exact final
counts. Version files stayed `0.20.0` (`Cargo.toml` workspace,
`src-tauri/tauri.conf.json`).

## P2 REAL engine reattachment (highest priority)

**Result: architecture holds. No silent restart, no tool replay.**

In-process coverage already existed
(`attached_view_reattaches_after_owner_restart_without_replay_or_duplicates`).
This pass added **real `target/debug/shadowcode serve` children**:

| Trial | Evidence |
| --- | --- |
| SIGKILL owner, ViewClient reattach | `jobs_started=0` `tools_replayed=0`; event ids preserved; no duplicates (`qualification_real.rs`) |
| Kill during `write_file` | file written once; restart does not increment the write counter |
| 3 attach/kill/reattach cycles | history intact; FD growth bounded (≤ +32) |
| Desktop SIGTERM while serve lives | lab: detached hang stayed `running` |
| Engine SIGKILL then new serve | recovered job status `interrupted`; no `running`/`queued` leftovers |
| Client SIGKILL while streaming | job not `completed` (still `running` on owner until cancelled) |

Lab artifact: `artifacts/qualification/lab.json`
`p2_reattach.desktop_death_left_task_running=true`,
`engine_kill_did_not_rerun=interrupted`.

`recover_jobs` marks interrupted work and does not execute tools. GET-only
reattach is unchanged. No auto-retry of POST.

## P3 Side-effect replay

**Result: completed mutations were not blindly repeated.**

| Side effect | Crash point | Replay class | After restart |
| --- | --- | --- | --- |
| `write_file` `side-effect.txt` | after create, during hung follow-up | `NeverAutoReplay` | content still `created-once`; write counter +1 only |
| `exec` `printf … > shell-side.txt` | after file exists | `RequiresConfirmation` | content still `shell-once`; not rewritten |
| In-process write-interrupt | mid-task SIGKILL | `NeverAutoReplay` | no second write |

Reads remain `SafeToReplay`. `git_clean` / `git_reset` are
`NeverAutoReplay` and require elevated + approval.

Background start/stop were exercised in P15; they are
`RequiresConfirmation` and are not auto-replayed by `recover_jobs`.

## P4 Million-event SQLite

**Result: 1M completed with no cutoff. No new indexes.**

Release example:
`cargo run -p shadowcode-core --offline --release --example qualification_sqlite -- N`

| N | insert_s | insert/s | db_bytes | peak_rss_kb | startup_ms | recent_ms | catch-up_ms | history_page_ms | search_ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 10k | 3.84 | 2604 | 1_712_128 | 6584 | 2 | 0 | 0 | 1 | 0 |
| 100k | 38.91 | 2570 | 16_195_584 | 7036 | 2 | 0 | 0 | 0 | 0 |
| 1M | 397.30 | 2517 | 164_319_232 | 7220 | 2 | 0 | 0 | 0 | 0 |

- `reached_requested_n=true`, `cutoff_s=null`, WAL, `telemetry=false`
- RSS rose from ~4.9 MB to 7.2 MB by 50k and **stayed flat through 1M**
- `history_page` still returns 128 events (product pagination)
- `EXPLAIN QUERY PLAN` uses `events_session_id` for recent and
  events-after (`qualification_real.rs`)
- **5M not run** (projected ~33 min at this rate). Not claimed.

Previous pass’s 60s debug probe (139k) is **not** comparable as a
throughput win; this is a different profile (release, no cutoff).

## P5 Extreme transcripts

**Result: measured. UI not virtualized.**

Reducer `applyEvent` on `model.delta` (Vitest, `QUAL_SCALE=1`, 78 s):

| Messages | reconstruct_ms |
| --- | --- |
| 10k | 167 |
| 50k | 17_635 |
| 100k | 64_344 |

`virtualized: false`. Desktop history is 128 events per page, so a 100k
in-memory reconstruct is **not** the shipped window path. 64 s for 100k
is unacceptable if the UI ever replayed the full tape at once; it does
not today. **No virtualization change** — measurements do not justify
rewriting the conversation list for the paged desktop.

Default `npm test` runs the 10k path only (`QUAL_SCALE` unset) so the
suite stays fast. Markdown/scroll were not separately instrumented beyond
existing `Markdown.test.tsx` and e2e.

## P6 Context pressure

**Result: keep-list retains the task under repeated compact.**

`preserve()` now records `decisions` in addition to intent, constraints,
unresolved/failures, files, plan, security, verification.

Tests:

- `keep_list_retains_objective_constraints_decisions_failures_and_plan`
- `repeated_compact_under_context_pressure_keeps_the_task` (8× compact
  at 4096 / 0.55 with full tool schemas)

Duplicate alias keys were **not** left in the compact JSON (they grew
the injected keep-list). `intent[0]` is the objective; `unresolved` /
`failed_approaches` are the unfinished/failure lists.

## P7 Long autonomy simulation

**Result: 1802 s idle `serve` monitor. Not 6 hours. Classification: ENVIRONMENT LIMITATION.**

`scripts/qualification/p7-monitor.mjs` sampled a live `shadowcode serve`
every 15 s for the requested **1800 s** (actual **1802 s**, 121 samples).
Six-hour interactive autonomy was not practical.

| Metric | First sample (2 s) | Last sample (1802 s) |
| --- | --- | --- |
| VmRSS | 58_364 KB | 47_276 KB |
| Threads | 17 | 17 |
| FDs | 35 | 35 |
| Children | 0 | 0 |

`rss_delta_kb=-11088` (released after warmup). `fd_delta=0`.
`leak_claimed=false`. **Cache is not called a leak.** A 6-hour session
remains unrun. Artifact: `artifacts/qualification/p7-monitor.json`.

## P8 Runaway

**Result: warn → replan → pause. Changing arguments continue.**

Policy: count 3 warn, 4 replan, 5 pause
(`autonomy::runaway_action`).

- Engine test `runaway_same_tool_pauses_but_changing_arguments_continue`:
  same `read_file` path fails with a loop/repeated-tool summary;
  `runaway.warning` events include `warn` and `replan`; diverse paths
  continue.
- Lab fixture: 5 identical `read_file` calls → job `failed`.

No false-positive shutdown of the changing-argument case.

## P9 Provider torture

**Result: existing + extended decoder tests. Tool calls stay correlated.**

Covered in `autonomy_021::provider_chaos_does_not_invent_output` and
`model_streams`:

- malformed / truncated JSON → error, no invented text
- missing tool id → synthesized `call_*`, name preserved
- repeated ids → non-empty ids, no empty correlation
- fragmented argument SSE
- HTTP 429 / 500 → no tools executed (`provider_http_429_and_500_do_not_execute_tools`)
- lab STREAM with null finish_reason: client kill ≠ success

OpenAI/Ollama/llama.cpp/LM Studio live quirk matrix was **not** each
spun as a separate vendor process. ollama is P10. Others remain
fixture-level. Classification for missing vendor processes:
**KNOWN LIMITATION** (harness coverage is the decoder, not every
vendor binary).

## P10 Real local model

**Result: harness survived. Model IQ not judged.**

`ollama list`: `gpt-oss:20b` (13 GB), `qwen3:14b` (9.3 GB).

Bounded task on `gpt-oss:20b` via
`http://127.0.0.1:11434/v1`: “Reply with exactly OK. Do not use tools.”

- process exit 0
- summary `OK`
- `usage_is_estimated=false`
- verification `claim=model_claim`, `verified` not implied by prose
- `max_steps=3`

This judges transport and verification honesty, not model quality.

## P11 Real disposable repo trial

**Result: explore → edit → test succeeded on a throwaway tree.**

Temp git repo with a broken `add.mjs` (`a - b`) and `add.test.mjs`.
Fixture model: `read_file` → `edit_file` → `exec node add.test.mjs`.
File became `a + b`. Job completed. **Not** user work.

## P12 Self-host

**Result: isolated worktree. Running binary not overwritten.**

`git worktree add --detach` at `5d33133` under `/tmp`.
`CARGO_TARGET_DIR` pointed at the scratch tree.
`cargo test -p shadowcode-core --offline --lib never_auto_replays`
exit 0. `stat` of
`…/ShadowCode/target/debug/shadowcode` unchanged. Worktree removed
with `--force` after the probe.

## P13 Git destruction

**Result: pre-existing work survived denied destructive Git.**

Real disposable repos with dirty, staged, untracked, rename, binary,
`--leading-dash`, unicode `雪.txt`, `file with spaces.txt`, detached
HEAD, merge conflict, and a linked worktree:

- workspace-level `git_clean` / `git_reset` **denied** (need elevated)
- elevated + **denied approval** does not run `git_clean`
- CLI fixture that asked for `git_clean`: dirty/unicode/spaces/committed
  files still present
- `parse_git_status` flags dirty/staged/untracked/conflict/detached and
  unusual names; worktree recovery `guess_paths=false`

## P14 Tool cancellation

**Result: cancel ≠ success. No owned orphans observed.**

- Lab: SIGINT on a CLI `run` → exit **130**
- Existing native tests: cancel/deny of `exec` is not `Success`; file
  not created; pending approval cleared
- Background `sleep 60` stopped via `background stop` / engine exit

MCP cancel is covered by existing MCP client tests (timeout/hang modes
reap fixture pids). A dedicated “cancel large sqlite query” OS probe
was not added; sqlite builtins use cooperative cancel in-process.

## P15 Background process torture

**Result: start + logs + engine stop cleaned the child.**

Lab: `background start` `printf ready; sleep 60`, logs contained
`ready`, pid alive, `serve` SIGTERM, pid then dead.
`cleaned_after_engine_stop=true`.

Port conflict, workspace delete, and restart-after-delete were **not**
each driven as separate OS scripts. Existing background tests plus this
real child are the evidence. Remaining cases:
**KNOWN LIMITATION** (not a contradictory result).

## P16 Hostile MCP

**Result: one bad server did not break doctor.**

Added `native/core/tests/fixtures/mcp-server.mjs` in `init_malformed`
mode via `shadowcode mcp add`. Doctor still exit 0 / runtime rust.
Existing `mcp_stdio` / `mcp_http` tests already reject oversize,
truncated, hang, and malformed initialize and reap children.

## P17 Accessibility

**Result: keyboard coverage on the test UI + a focus-restore unit test.
Not a full native-desktop keyboard-only day.**

- New `Dialog.focus.test.tsx`: opener focus restored on unmount
- Playwright e2e (7/7): settings dialog labels, Tab trap, Escape,
  Ctrl+K / Ctrl+N / Ctrl+,
- Light/dark axe wcag2a/aa on home

The e2e server is `scripts/serve-test-ui.py`, not the GTK WebKit
window. Classification: **ENVIRONMENT LIMITATION** for “keyboard-only
native desktop.” No UI redesign.

## P18 Native build from clean source

| Command | Result |
| --- | --- |
| `npm --prefix ui run typecheck` (`tsc --noEmit`) | pass (after dropping Node `process` types from the scale test) |
| `npm --prefix ui run build` | pass |
| `npm --prefix ui test` | **40 passed** (10 files) |
| `npm --prefix ui run test:e2e` | **7 passed** |
| `cargo fmt --all -- --check` | pass via `tools/rust-dev/extracted/usr/bin` |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | pass (fixed unused `Server.requests` in the new OS-process test) |
| `cargo test --workspace --offline --lib --tests --bins` | **282 passed** (see P28) |
| `cargo build` desktop release (packaging) | pass |
| rustfmt/clippy on PATH | **not present**; extract used; no sudo |

`goal_verification_uses_the_tester_route_and_executes_its_acceptance_command`
failed once under parallel load (4127 vs 4096 tester context) and
**passed on immediate re-run**. Treated as a tight-budget flake, not a
qualification rewrite of the 4096 tester route.

## P19 AppImage

**Result: built, inspected, extract-mode smoked. Isolated profile only.**

- `ShadowCode_0.20.0_amd64.AppImage` 87_620_088 bytes
- SHA-256 `c12520e9edd0d6f4f5173446d03bec86ecb4f016d3d8a0a17c944af2ac0ebb96`
- First `linuxdeploy` failures: PATH contained `/snap/bin` (missing) and
  `/usr/local/bin/node` → `/root/.hermes/node/bin/node` (**Permission
  denied**). That host workaround is **replaced**: packagers now construct
  PATH themselves (`scripts/native-packaging-env.mjs`). **No sudo.** The
  human does not sanitize PATH. Dirty-PATH proof:
  `node --test scripts/test-native-packaging-env.mjs` (see addendum).
- `--appimage-extract-and-run --version` → `ShadowCode 0.20.0`
- Isolated `--json doctor` exit 0
- `--appimage-extract` contains `usr/bin/shadowcode` and desktop file
- Window launch with `DISPLAY` for 4 s then SIGTERM: process was alive
- **Did not** install into `~/Applications`

`node scripts/check-native-package.mjs` passed (no Python/Node sidecars;
notices + runtime sources present).

## P20 Debian

**Result: built and extracted under `/tmp`. Not installed on the primary system.**

- `ShadowCode_0.20.0_amd64.deb` / package name `shadow-code` **0.20.0**
- 12_498_040 bytes
- SHA-256 `97fc89b5637ec5e92744cbb3057fa6dddc3de23de8219f607ca4151af5c10989`
- Depends: `git, libwebkit2gtk-4.1-0, libgtk-3-0` (runtime, not `-dev`)
- `dpkg-deb -x` → `/usr/bin/shadowcode`,
  `/usr/share/applications/ShadowCode.desktop` (`Icon=shadowcode`),
  `usr/share/icons/hicolor/{32,128,256,512}x512/apps/shadowcode.png`
- Primary `~/Applications/ShadowCode-0.20.0-x86_64.AppImage` left in
  place. **No `dpkg -i`.**

Uninstall of a system install was not exercised (we never installed).

## P21 Installer / update / checksums

**Result: existing policy recorded. Not changed.**

`scripts/install-appimage.sh` + `scripts/test-install-appimage.sh`
(isolated `$HOME`):

| SHA256SUMS | Behavior |
| --- | --- |
| present + match | “Verified SHA-256…” and install |
| present + mismatch | refuse; exit 1; previous install left in place |
| absent | warn `install is not checksum-verified` and **still install** |

Replacement of `0.19.0` with `0.20.0` and profile preservation:
`test-install-appimage.sh` passed. Checksum policy was **not** tightened
or loosened.

## P22 Clean-machine assumptions

| Assumption | This host / package |
| --- | --- |
| glibc | Ubuntu 2.39 |
| git | 2.43.0; Debian Depends includes `git` |
| Desktop | debug binary links WebKitGTK 4.1 + GTK 3; deb Depends the runtime `.so` packages |
| FUSE | `/dev/fuse` + `fusermount3` present; installer/AppImage support extract-and-run without FUSE |
| Config/state | `--profile` / `<profile>/config/config.yaml` / `<profile>/state/shadow-agent.db` |
| `-dev` packages | host has `libwebkit2gtk-4.1-dev` for **building**; packaged deb does **not** Depend on `-dev` |
| Providers | none required at startup; ollama optional at `127.0.0.1:11434` |

linuxdeploy PATH scan was a **build-host** hazard; the packager now
ignores the caller PATH. Still not a runtime Depend.

If a future builder lacks WebKit/GTK headers, the apt line (do not run
here without the user) is:

`sudo apt install libwebkit2gtk-4.1-dev libgtk-3-0 libwebkit2gtk-4.1-0`

## P23 Failure recovery

**Result: no false success after kill. No silent mutation replay.**

| Kill | Restart observation |
| --- | --- |
| Engine during write | interrupted; file not rewritten |
| Engine during hang | jobs not `running` |
| CLI during STREAM | not `completed` |
| Desktop during hang | task still `running` on serve |
| Compact / verification | existing recover_jobs + verification.summary tests |

State files were SQLite WAL; doctor `database` check passed on fresh
profiles. No corruption probe found a torn DB in these runs.

## P24 Verification engine

**Result: model prose is not verified.**

Lab CLAIM (“All tests passed…” with no tools):
`verification.summary` payload

- `verified: false`
- `unverified_claim: true`
- `status: not_run`
- `model_claimed_success: true`
- note: “Model text is not verification…”

Engine test
`model_success_claim_without_tests_is_not_verified` also passes.
P10 ollama “OK” did not flip `verified`.

## P25 Doctor

**Result: local only. No telemetry product.**

| Condition | Observation |
| --- | --- |
| Healthy isolated profile | `runtime=rust`, checks include config/db/git |
| Provider endpoint `127.0.0.1:1` | `model-response` `not_checked`; doctor still runs |
| Invalid workspace | CLI exit 1 `Workspace does not exist` |
| Empty PATH | doctor still returns JSON (git check degrades; no crash) |
| JSON dump | contains the word `telemetry` as a **false/local-stats** field, not an upload |

Background-issue doctor was not separately broken; P15 cleaned children.

## P26 Perf vs pre-0.21

**No improvement claimed.**

Comparable numbers from the autonomy journal: debug 60s probe inserted
139_305 events. This pass’s **release** 1M insert is a different binary
and a different stop condition. UI 10k reconstruct 167 ms vs the
journal’s “under 4s” 10k replay is the same order; not advertised as a
win. Startup/listing after 1M were 0–2 ms on existing indexes.

## P27 Second audit of qualification changes

Reviewed only this branch’s qualification diff vs `5d33133`:

**Kept**

- `preserve()` `decisions` extraction
- OS-process tests, git destruction tests, sqlite example
- Lab / remaining / doctor / p7 / package-smoke scripts
- Dialog focus test; scale test gated by `QUAL_SCALE`
- rustfmt-only wraps in `engine.rs` / `inspection.rs`

**Removed / avoided**

- Duplicate keep-list aliases that bloat compact notes
- UI virtualization
- Extra SQLite indexes
- Checksum policy change
- Version bump
- Debug leftovers in the product (scripts are under
  `scripts/qualification/`; artifacts gitignored)
- Root `node_modules/` from a mis-aimed vitest run (deleted, not
  committed)

Clippy `-D warnings` required reading `Server.requests` in the new
OS-process test (assertion that the fixture model was actually hit).

## P28 Complete gate from clean tree

Exact counts after the qualification tests existed. **Unrun tests are
not marked passed.**

| Suite | Command | Count |
| --- | --- | --- |
| Rust workspace lib/tests/bins | `cargo test --workspace --offline --lib --tests --bins` | **282 passed**, 0 failed, 0 ignored |
| UI unit | `npm --prefix ui test` | **40 passed**, 10 files |
| UI e2e | `npm --prefix ui run test:e2e` | **7 passed** |
| Native CLI script | `node scripts/test-native-cli.mjs` | 21 scenario groups, 32 model requests |
| AppImage installer script | `bash scripts/test-install-appimage.sh` | pass |
| QUAL_SCALE reconstruct | `QUAL_SCALE=1 vitest … transcript.scale.test.ts` | 1 passed (78 s) |
| 5M sqlite | not run | — |
| 6 h leak | not run | — |
| `dpkg -i` | not run | — |

Clean-tree re-run on `c8c1e0f` (this report’s commit parent after the
harness/docs land; working tree empty): Rust **282 passed / 0 failed /
0 ignored**; UI **40 passed / 10 files**. Unrun: 5M sqlite, 6 h leak,
`dpkg -i`.

## P29 Release decision

**Do not auto-release. Do not merge to main. Human decision required.**

| Class | Items |
| --- | --- |
| **RELEASE BLOCKER** | None confirmed against the 0.21 *architecture* on this host. Silent replay, false verification, and casual git destruction did **not** reproduce. |
| **HIGH PRIORITY NON-BLOCKER** | 4096-token tester-route flake under load (not re-run this addendum); 100k full-tape reconstruct cost if a future UI path replays everything; denial vs cancel still share an error string (pre-existing). linuxdeploy PATH hygiene is **fixed in the packager** (see addendum). |
| **KNOWN LIMITATION** | Shell policy is a word list; compact is O(n); 5M events not inserted; P15 port-conflict/workspace-delete matrix incomplete; vendor-specific provider processes beyond ollama+fixtures. |
| **ENVIRONMENT LIMITATION** | 6-hour leak hunt not run this addendum; keyboard-only **native GTK** session not run this addendum; `dpkg -i` skipped to protect the primary install; rustfmt/clippy not on default PATH (extract used). Host Hermes node hijack remains on disk; packaging no longer reads that PATH entry. |
| **FUTURE WORK** | Virtualize only if a product path reconstructs ≥50k messages in the window; optional 5M/6h CI job; capability-based tool hiding (already rejected this cycle). |

A human may still ship 0.21 later. This report **rejects** treating
qualification as an automatic release.

## P30 Version

**Keep 0.20.0.** Gates remaining unresolved (P7 duration, P5 virtualization
decision, P17 native a11y, P29 human sign-off) do not justify rewriting
crate/UI/package versions to 0.21.0.

Confirmed still `0.20.0`: workspace `Cargo.toml`,
`src-tauri/tauri.conf.json`, AppImage `--version`, Debian `Version:`.

## Commands a human can replay

```sh
export PATH="~/Documents/Codex/2026-09-19/new-chat/work/tools/rust-dev/extracted/usr/bin:$PATH"
cargo test --workspace --offline --lib --tests --bins
npm --prefix ui test
QUAL_SCALE=1 npm --prefix ui exec -- vitest run src/lib/transcript.scale.test.ts --testTimeout=120000
cargo run -p shadowcode-core --offline --release --example qualification_sqlite -- 1000000
node scripts/qualification/lab.mjs
node scripts/qualification/remaining.mjs
# packaging: caller PATH may be dirty; the script sanitizes it
PATH="/usr/local/bin:/snap/bin:/usr/bin:/bin" APPIMAGE_EXTRACT_AND_RUN=1 \
  node scripts/build-native.mjs
node --test scripts/test-native-packaging-env.mjs
```

## Packaging PATH addendum (2026-09-21 later)

**Gate: packager PATH hygiene. Version still 0.20.0. Main untouched.**

| Check | Result |
| --- | --- |
| Reproduced host failure | `PATH=/usr/local/bin:/snap/bin:/usr/bin:/bin` + `linuxdeploy --list-plugins` → exit 127, `Permission denied: "/usr/local/bin/node"` |
| Sanitizer | `scripts/native-packaging-env.mjs` builds PATH; does not inherit caller PATH |
| Applied in | `build-native.mjs` (before cargo/Tauri/linuxdeploy), `native-runtime.mjs` `buildRuntime`, `build-linux.sh` |
| Dirty-PATH test | `PATH="/usr/local/bin:/snap/bin:/usr/bin:/bin" node --test scripts/test-native-packaging-env.mjs` |
| Full AppImage/deb rebuild this addendum | not required to prove the PATH scan; existing 0.20.0 bundles left in `target/`; `~/Applications` not written |
| 4096-token flake / native a11y / longer soak | not re-run; not claimed |

## Stop condition

Qualification evidence is sufficient to **reject a 0.21.0 bump and
reject auto-release**, and sufficient to show the architecture survived
reality on this machine. This branch should be pushed and left unmerged.
