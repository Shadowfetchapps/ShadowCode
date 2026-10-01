# ShadowCode 0.21 merge-readiness report

**Do not auto-merge. Do not auto-release. Human decision required.**
Package and crate version remain **0.20.0**.

| Field | Value |
| --- | --- |
| Date | 2026-09-21 |
| Repo | `~/Documents/Codex/2026-09-19/new-chat/work/ShadowCode` |
| GitHub | https://github.com/ShadowfetchLinux/ShadowCode.git |
| BASE QUALIFICATION COMMIT | `c4efa493d78f68873eca9b3b4d86c63e6c96bd76` |
| Qualification ancestry | includes `7970c45` and PATH sanitizer `c4efa49` |
| Branch | `release/shadowcode-0.21-merge-ready` |
| FINAL CANDIDATE COMMIT | `d8c86baa831a44118a58973c49a3bf0b7ef9fcab` |
| Main | not checked out, not merged, not force-pushed; `origin/main` stayed `ab5e7c8` |
| Qualification branch | left in place |
| VERSION DECISION | **keep 0.20.0** |

## TESTER-ROUTE FLAKE RESULT

**Root cause: compact keep-list inflation, not a too-small 4096 window.**

Measured components for the goal-verification tester turn (builtin schemas,
short previous milestone, isolated tempfile path):

| Component | Estimated tokens |
| --- | --- |
| Messages (system + prior user/assistant + current user) | 483 |
| Builtin tool schemas | 2778 |
| Hard reserve (`messages + schemas + 512`) | 3773 |
| Compact quarter-window hard_limit | 38 |
| After keep-list compact (short path) | 3965 |
| Account used / remaining | 3839 / 257 |

`4096` is **not too small by design** for this fixture: the live request fits
(3773 ≤ 4096). Compact is eager (it reserves 25% for output). On this catalog
that always triggers, and the 1200-byte keep-list note can consume the remaining
headroom. Under parallel load a slightly longer workspace path or keep-list
produced the observed `4127` vs `4096` failure; an immediate rerun often
passed.

**Fix (smallest product defect):** if the original messages already satisfy
`response_budget`, compact must not replace them with a prompt that fails that
check. The tester-route `context_limit` stays 4096. The limit was not raised
to hide the flake.

**Reproduction after the fix**

| Load | Result |
| --- | --- |
| Alone ×20 | 20 passed |
| `model_routing` ×5 with `--test-threads=8` | 5/5 passed |
| Workspace `--test-threads=16` | 287 passed, 0 failed |
| Workspace `--test-threads=32` | 287 passed, 0 failed; tester-route ok |

Regression: `native/core/tests/tester_route_budget.rs` (component measurement,
short-path compact, long-path compact, true overflow still errors, live
4096 tester goal on a long workspace path).

## PACKAGING PATH RESULT

Existing `c4efa49` helper **meets the gate**. Verified, not rewritten.

- PATH is constructed; caller PATH is not inherited
- No hardcoded developer home; Node is `process.execPath`’s directory
- Drops `/usr/local/bin`, `/snap/bin`, Hermes, `/root` hijacks
- No root, no user global PATH change
- Ordinary `/usr/bin` + `/bin` remain
- Missing `node` fails clearly from `native-packaging-env.sh`

Acceptance: AppImage+deb built from a clean worktree of `79bea63` with dirty
caller PATH `/usr/local/bin` → `/root/.hermes/node/bin/node` **without**
manual PATH edit. Packager logged
`Using sanitized packaging PATH: /tmp/sc-021-merge-ready-gate/target/debug:…nvm…:/usr/bin:/bin:/usr/sbin:/sbin`
and omitted `/usr/local/bin`. Host Hermes link left on disk.

`PATH="/usr/local/bin:/snap/bin:/usr/bin:/bin" node --test scripts/test-native-packaging-env.mjs`:
**4 passed**, including live `linuxdeploy --list-plugins` after the bundle
cache existed. Before linuxdeploy was cached: 3 passed, 1 skipped (honest).

## APPIMAGE RESULT

Three dirty-PATH AppImage+deb builds (two earlier this pass, one from the
clean worktree). Isolated smoke only. `~/Applications` untouched
(primary `ShadowCode-0.20.0-x86_64.AppImage` mtime 2026-09-20 22:53).

| Run | SHA-256 | Bytes | `--version` |
| --- | --- | --- | --- |
| 1 | `35f836245ce9c7eec026d073776f2ffae1c0a6598d683032e51835162c9c9196` | 87_620_088 | ShadowCode 0.20.0 |
| 2 | `d4fe7f2118fb3188aae9ebf774d11fba3240d52ab23339108115ded9058c7e33` | 87_620_088 | ShadowCode 0.20.0 |
| 3 (clean worktree) | `315cf14d2dc0f99e6f04f44ad121ce72542c059a4e34ef18d1d1db7da3c89e76` | 88_877_560 | ShadowCode 0.20.0 |

Checksums differ across runs (AppImage runtime MD5 embed / squashfs). The
**process** is repeatable without PATH surgery; bit-identical artifacts are
not claimed. `check-native-package.mjs` passed (ELF, no Python sidecars,
notices, matching AppImage/deb `.text`). Isolated extract-and-run doctor
exit 0. Isolated window launch 4 s then SIGTERM: alive.

## DEBIAN RESULT

| Run | SHA-256 | Bytes | Version |
| --- | --- | --- | --- |
| 1 | `fe274ee14eef51fd143cea78c178865dc58db14b728601aded420c35bf1646f2` | 12_498_160 | 0.20.0 |
| 2 | `55338f7252b9d3a640fe7623456c3129ff80eeaa9accd4c40b95d59b6f403daa` | *(rebuilt)* | 0.20.0 |
| 3 (clean worktree) | `4cf2b35be6376e8761d45d73994c8a77471a3b8b3b88932a25570f1718053ebb` | 12_498_202 | 0.20.0 |

Package name `shadow-code`. Depends:
`git, libwebkit2gtk-4.1-0, libgtk-3-0` (runtime, not `-dev`).
Extracted under `/tmp` only: `/usr/bin/shadowcode`,
`ShadowCode.desktop` (`Icon=shadowcode`),
`hicolor/{32,128,256,512}x512/apps/shadowcode.png`.
**No `dpkg -i`.**

## NATIVE KEYBOARD ACCESSIBILITY RESULT

**ENVIRONMENT LIMITATION.** A primary
`~/Applications/ShadowCode.AppImage` window was already open. `xdotool search
--name ShadowCode` matched existing X11 windows; key delivery cannot be
attributed to an isolated disposable profile. Further automated keys were
refused. Playwright e2e (7/7) is **not** treated as equivalent. No a11y
product change.

## RESOURCE SOAK RESULT

Active soak (`scripts/qualification/p6-active-soak.mjs`): periodic tasks,
history, jobs, file reads, bounded `exec`, cancel, background start/stop,
connect/disconnect, compact-pressure prompts against a fixture model.

**Exact duration: 3696 s (61.6 min).** Requested 21600 s (6 h). Stopped
with SIGTERM so this pass could finish. Classification: **ENVIRONMENT
LIMITATION** for a 6-hour session. Cache/SQLite growth after activity is
**not** called a leak.

| Metric | First sample (0 s) | Last sample (3695 s) |
| --- | --- | --- |
| VmRSS | 64_356 KB | 69_856 KB |
| VmSize | 1_356_056 KB | 1_425_708 KB |
| Threads | 17 | 18 |
| FDs | 35 | 35 |
| Children | 0 | 0 |
| DB | 163_840 B | 9_687_040 B |
| SQLite FDs | 4 | 4 |

`rss_delta_kb=+5500` after warmup. `fd_delta=0`. `thread_delta=+1`.
`activity_ok=1521` `activity_err=0` `cycles=169` `model_hits=1012`.
`leak_claimed=false`. Artifact: `artifacts/qualification/p6-active-soak.json`.

## 5M EVENT RESULT IF EXECUTED

**Executed. Optional gate. Not a release requirement.**

`cargo run -p shadowcode-core --offline --release --example qualification_sqlite -- 5000000`

| Metric | Value |
| --- | --- |
| reached_requested_n | true |
| cutoff_s | null |
| insert_s | 2159.27 |
| insert/s | 2316 |
| db_bytes | 832_266_240 |
| peak_rss_kb | 7184 (flat from 50k through 5M) |
| startup_existing_ms | 6 |
| recent_events_ms | 0 |
| history_page_ms | 0 (128 events) |
| events_after_ms | 1 |
| session_search_ms | 0 |
| journal | WAL, `telemetry=false` |

## REAL LOCAL MODEL CODING RESULT

Disposable git repo, `gpt-oss:20b` via `http://127.0.0.1:11434/v1`.
Task: inspect broken `add.mjs` (`a - b`), edit, run `node add.test.mjs`.

- Job `completed`, 4 steps, `usage_is_estimated=false`
- File became `a + b`; independent `node add.test.mjs` exit 0 / `ok`
- `verification.claim=verified` from recorded `node add.test.mjs` exit 0
- `unverified_claim=false`; model prose was not the verifier
- Harness judged, not model IQ. Artifact:
  `artifacts/qualification/p8-local-model-coding.json` (gitignored)

## Quality gate counts

Unrun checks are not marked passed.

Clean worktree `/tmp/sc-021-merge-ready-gate` at `79bea63` plus the live-WAL
writer fix (python3). `cargo test --workspace --offline --lib --tests --bins`
requires `target/debug/shadowcode` first (`qualification_real` asserts the
file). Isolated `CARGO_TARGET_DIR` is **not** equivalent: those tests look
at `native/core/../../target/debug/shadowcode`.

| Suite | Command | Count |
| --- | --- | --- |
| RUST TEST COUNT | `cargo build -p shadowcode-desktop --offline --locked` then `cargo test --workspace --offline --lib --tests --bins -- --test-threads=16` | **287 passed**, 0 failed, 0 ignored, 32 suites |
| Prior stress (dirty tree, 16- and 32-thread) | same cargo test filter | **287 passed**, 0 failed |
| UI TEST COUNT | `npx vitest run` / `npm --prefix ui test` | **40 passed**, 10 files |
| E2E COUNT | `npx playwright test` (worktree needed `.venv` symlink for `serve-test-ui.py`) | **7 passed** |
| CLIPPY RESULT | `cargo clippy --workspace --all-targets --offline -- -D warnings` | pass (rust-dev extract) |
| RUSTFMT RESULT | `cargo fmt --all -- --check` | pass (rust-dev extract) |
| TYPESCRIPT RESULT | `npx tsc --noEmit` | pass |
| UI production build | `npx vite build` | pass |
| NATIVE BUILD RESULT | `tauri build --no-bundle` inside `build-native.mjs` | pass |
| AppImage | dirty-PATH `node scripts/build-native.mjs` | pass; `--version` ShadowCode 0.20.0 |
| Debian | same build | `shadow-code` 0.20.0; no `dpkg -i` |
| Package inspect | `node scripts/check-native-package.mjs APPIMAGE DEB` | pass |
| Isolated AppImage smoke | `node scripts/qualification/package-smoke.mjs` | doctor exit 0; window 4 s alive; `~/Applications` untouched |
| Doctor smoke | isolated `--profile` `--json doctor` on debug `shadowcode` | exit 0 |
| Installer checksums | `bash scripts/test-install-appimage.sh` | pass (disposable `$HOME`) |
| Packaging env | `node --test scripts/test-native-packaging-env.mjs` | **4 passed** after linuxdeploy cache |
| Native GTK WebDriver suite | `node scripts/test-native-desktop.mjs` | **not run** (primary AppImage already open) |
| 6 h soak | requested 21600 s | **not completed** (3696 s active) |
| `dpkg -i` | — | **not run** |

## VERSION DECISION

**Keep 0.20.0.** No confirmed architecture blocker, but this pass did not
complete a 6-hour soak or an isolated native keyboard-only day. Bumping
crate/UI/package/AppImage/Debian to 0.21.0 is a human release act, not an
automatic consequence of the branch name.

## CONFIRMED DEFECTS FIXED

- Compact could fail a 4096-token tester request that already fit, by
  inserting a keep-list note (the parallel-load flake).
- `live_wal_commits_from_another_process_are_visible_without_database_changes`
  failed on a sanitized PATH (`/usr/bin/node` v18 has no `node:sqlite`).
  That is a test-host defect, not a product SQLite bug. The writer is now
  `python3` + stdlib `sqlite3`. Alone 8/8 and `--test-threads=8` ×12 on
  `PATH=/usr/bin:/bin`. The 4096 limit was not involved.

## REMAINING HIGH-PRIORITY ISSUES

- 100k full-tape UI reconstruct (~64 s) if a future path replays the entire
  conversation in memory (desktop still pages 128 events)
- Denial vs cancel still share an error string (pre-existing)
- AppImage/deb checksums are not bit-stable across rebuilds

## KNOWN LIMITATIONS

- Shell policy is a word list, **not an OS sandbox**
- Compact is O(n) removals with a full JSON estimate each time
- Vendor-specific provider processes beyond ollama + fixtures
- P15 port-conflict / workspace-delete matrix still incomplete
- 4096-token local models have little headroom once the full catalog is
  attached; that is measured, not hidden

## ENVIRONMENT LIMITATIONS

- 6-hour leak hunt not completed this pass
- Isolated native keyboard-only GTK session blocked by an already-open
  primary AppImage window
- `dpkg -i` skipped to protect the primary install
- rustfmt/clippy not on default PATH (rust-dev extract used; no sudo)
- Host Hermes node hijack remains on disk; packaging no longer reads it
- Ubuntu `/usr/bin/node` v18 cannot run `node:sqlite` writers; sanitized
  PATH tests must not assume nvm Node 22
- `qualification_real` needs a prior `cargo build` of `shadowcode`; a
  blank tree with only `cargo test --lib --tests --bins` is 3 missing-binary
  failures, not product regressions
- Full `test-native-desktop.mjs` WebDriver suite not run this pass

## RELEASE BLOCKERS

**NONE CONFIRMED** against the 0.21 architecture on this host. Silent replay,
false verification, and casual git destruction did not reproduce in
qualification. The tester-route flake has a root-cause fix. Packaging no
longer requires manual PATH surgery.

## RECOMMENDED HUMAN ACTION

**MERGE CANDIDATE** for human review of `release/shadowcode-0.21-merge-ready`.
This is not an automatic merge and not a release. Leave version **0.20.0**
until a human explicitly accepts the documented limitations and bumps.

## BRANCH

`release/shadowcode-0.21-merge-ready`

## COMMIT SHA

`d8c86baa831a44118a58973c49a3bf0b7ef9fcab` (evidence + WAL fix). Branch tip after this SHA note is the pushed HEAD.

## SECOND AUDIT (merge-ready vs qualification `c4efa49`)

Justified: compact restore-if-original-fits; tester-route budget tests;
python3 live-WAL writer; qualification p5/p6/p8 scripts; changelog;
this report; journal addendum. No product UI/features. No hardcoded
developer home. No debug leftovers in product code. Qualification
branch and evidence left in place.
