# Flagship implementation ledger

Goal: a dependable, easy-to-use native workspace combining supported vendor login/session routes and compatible local models, with trustworthy execution, review, recovery, and releases. Popularity is an aspiration, not an engineering acceptance result.

Baseline: `e371baaa4c690527173a322f7e0e1cfae4db329a`. Implementation branch: `flagship/reliability-foundation`. Preserve the existing architecture and provider integrations. The complete [acceptance specification](FLAGSHIP_ACCEPTANCE_SPEC.md) remains in scope; this ledger is not a narrowed definition of completion.

## Implemented increments

- REMOTE-01 — remote background commands respect the terminal switch. Commit `d488fd2`. Real HTTP regression trusts the project, proves denied requests register no process and write no marker, then proves enabling the switch permits execution. All nine remote integration tests and seven remote policy unit tests pass. Installed release is unchanged; the new test exercises source-built core services.
- CLI-MCP-01 — distinguish enabled discovery from approved invocation. Commit `0286d36`. Smoke verifies initialize/list, no tools/call unattended or while approval is pending, exactly one approved call with exact arguments, and process cleanup. All 21 CLI scenario groups pass against the baseline debug executable (32 scripted model requests). This is test correction, not a claim of live provider authentication.
- CONTEXT-01 — remove count-triggered compaction and the 20-byte keep-list truncation. Boundary checks cover 20, 69, 70, 71, and 200 messages. All 16 autonomy integration tests and ten model-quality tests pass, including real token-pressure fixtures, retained constraints, and summary fallback. Optional request excerpts are reduced when the summary itself exceeds the context budget. This is source-level verification, not an installed AppImage update.

- PRF foundation — versioned native receipts now distinguish commands from configured checks and bind passing evidence to content fingerprints. Engine completion, explicit check tasks, required goal milestones, Compare counts, and UI use the distinction. Source tests cover the original false-positive, stale post-check edits, same-check recovery, and unrelated failure retention. [Semantics and remaining integration](VERIFICATION_RECEIPTS.md). Historical freshness now has a follow-up implementation described below; continuous invalidation remains open.

Verification checkpoint validation: broad source run passed 325 native tests (two pre-existing ignored), including engine, goals, Compare, service commands, and core unit tests. The final execution-failure addition passed all four receipt tests and eight goal tests. UI: 309 tests and production build passed. Core all-target Clippy with warnings denied and native desktop build passed on the final source. Native CLI passed all 21 scenario groups. The final rebuilt executable reproduced `printf test` with `verified=false`, `status=not_run`, and `unverified_claim=true`. All provider responses were local fixtures; no paid/live authentication or release package was exercised.

## Historical freshness increment

Historical verification is reassessed without mutating original receipts. Visible UI summaries refresh on mount/focus/manual request and suppress prior passing status while checking or unavailable. Compare checks lane content before acceptance, requires explicit review of stale checks, and reserves the selected lane against new application-owned tasks. Explicit check tasks now select their session using the existing selection-generation guard so approval controls target the right task.

Recorded validation: six Compare and fourteen service tests pass; 312 UI tests and production build pass. These exercise fixture/native service behavior, not live provider authentication or packaged Wayland acceptance. Recovery journaling, cross-process exclusion, unsaved buffers, continuous freshness and efficient shared indexing remain unfinished.

## Compare outcome and cleanup boundary

Keep now persists the applied result and scoreboard before removing lanes. Discard persists its logical outcome before disposal. An additive `cleanup_pending` field distinguishes a durable result from remaining cleanup. A stop failure retains all remaining lanes; each disposal also checks task state and reserves an existing lane against application-owned work. The UI reports retained copies and supports cleanup retry.

A native regression holds a lane reservation during Keep, restarts the service with cleanup pending, edits the applied file, then retries cleanup twice. The result survives, later edits remain, and the win is counted once. This covers the post-receipt recovery path, not a crash immediately after patch application. The full intent/preimage/postimage journal, per-lane durable checkpoints, process-tree exit proof, cross-process mutation coordination and lock-scope redesign remain required.

## Compare durable intent foundation

Before source mutation, Keep captures the working-tree preimage using a temporary index, pins it under `refs/shadowcode/recovery/<compare-id>`, and persists an operation identity, selected model, result commit, source HEAD, changed paths and `applying` phase in the existing revision-checked record. The public state is `needs_review` until application succeeds and the applied outcome is persisted. A confirmed Git preflight refusal returns to `done` with `not_applied`; an uncertain failure retains recovery material and blocks Keep/Discard. The UI exposes the interrupted state and project Changes.

Native coverage includes preimage pin/content checks and restart from seeded interrupted records with unchanged and edited files. Seeded interruption is not an OS-crash injection demonstration. Full reconciliation, expected postimages, index identity, guided recovery resolution, cross-process project ownership, per-path bounds/redaction and bounded recovery retention are still required. Pins are intentionally retained in this increment to avoid destroying recovery evidence; they may retain nonignored source content just as the existing Compare snapshot does. Do not label this foundation complete crash recovery.

## Compare exact-state reconciliation

New operations record the expected postimage tree (computed by applying the binary patch to an isolated index), source branch/ref and staged-content tree. The recovery action checks branch, HEAD and staged content, then compares current source content to the preimage and expected postimage. An exact postimage persists the applied outcome and win once; an exact preimage returns the comparison to a retryable state. Ambiguous content and incomplete legacy evidence stay blocked. No recovery action applies/reverts files or deletes lanes. The UI exposes Recheck recovery alongside Changes.

Validation: nine native Compare tests cover before/after/ambiguous/staged-change restart states and repeated assessment; ten Compare UI tests cover the action; strict Clippy and production UI build pass. Restart cases seed journal states rather than killing a live application. Full crash injection, simultaneous external edits, guided ambiguous-state resolution, bounded recovery retention and cross-process ownership remain open. Whole-tree comparison is conservative: unrelated edits also require review. Index comparison uses staged-content identity, not all index flags or unsaved editor buffers.

## Actual Compare process-crash coverage

Linux integration tests now spawn an isolated service test process and intercept only the source Git apply/removal commands through a test-local PATH wrapper. The parent sends SIGKILL before patch application, after patch application, and after outcome persistence before the first worktree removal. Restart uses the actual journal written by Keep, not a seeded replacement. The test checks before/applied reconciliation without reapplication, preserved lane copies, binary bytes, rename/delete results and executable mode. Crash controls exist only in the test executable; production has no environment-triggered fault injection.

The Linux suite reports eleven passing tests, including the no-op child entry used by the parent harness. This covers three real process-crash boundaries, not every journal write, disk-full failure, power-loss/fsync behavior or concurrent external edit. Those acceptance cases, ambiguous recovery UX and cross-project synchronization remain open.

## Compare project coordination

Compare metadata synchronization is now scoped to canonical project paths. A short registry mutex only manages weak lock references and is never held across async work. Start/Keep/Discard/recovery also acquire a nonblocking OS file lock in Git's common directory, shared across app profiles/processes and related worktrees. Existing revision-checked persistence and engine reservations remain. Lock order is documented; the repository lock file is never unlinked. Unix opens reject symlinks, nonregular files, other owners and extra hard links.

The Compare suite covers ownership refusal before source changes and successful retry after release. The real-crash parent is a second process and verifies that it cannot acquire the child's repository ownership at all three mutation boundaries. A unit test holds project A's metadata lock while acquiring project B's. This establishes scoped-lock behavior, not a measured end-to-end UI latency target. Git/worktree subsystem locks, independent external editors, advisory-lock cooperation by older app versions and network-filesystem lock semantics remain limitations.

## Release draft staging and immutable retry

The release workflow preserves all existing gates and delegates publication to `scripts/publish-native-release.mjs`. It stages a draft, uploads only missing assets without clobber, downloads all expected assets to verify SHA-256 bytes and completeness, then publishes. Published retries are read-only; changed bytes, unexpected assets, incomplete published releases and authorization failures are refused. A deterministic manifest binds tag/source commit, architecture, Cargo/UI lockfiles, runtime pin and package hashes. Per-tag workflow concurrency serializes cooperating release runs. Native PR CI and release CI run the publisher fixture suite.

Eight local Node tests exercise failure/retry and corruption paths; both workflow YAML files parse. No GitHub draft or public release was created to test this change. Remote tag protection, platform signing/authenticated updates and full workflow execution remain unverified. Manual concurrent repository edits are outside workflow concurrency. Draft discovery follows GitHub's authenticated [List releases API](https://docs.github.com/en/rest/releases/releases#list-releases); publication commands were checked against installed `gh` help.

## Provider runtime capability enforcement

The ACP execution adapter now checks the live initialize response before starting a session: only supported protocol version 1 is accepted, and queued image prompts require explicitly advertised image support. Follow-up images are checked at send time too. Missing, false or malformed image declarations cannot silently send attachments or trigger a provider/billing fallback. The image capability resets at process startup; cached catalog/vendor-name assumptions cannot authorize actual image transmission.

Parameterized fixtures cover Cursor, Antigravity and Grok with queued/follow-up images and true/false/missing/malformed declarations, plus unsupported/missing protocol versions. Existing Antigravity Google authentication and session/model-switch fixtures remain exercised. This is protocol conformance evidence, not a live login test or a change to the supported vendor login routes. The existing Grok early vision restriction and broader capability/UI synchronization remain separate work. Reference: [ACP v1 initialization](https://agentclientprotocol.com/protocol/v1/initialization).

## Managed local runtime admission foundation

Different-model requests now wait cancellably for active model leases instead of failing immediately. The last lease wakes waiting requests, and notification registration precedes the lease check to avoid lost wakeups. Existing same-model sharing remains. Cancellation and runtime shutdown prevent a waiting request from starting another model. All existing callers of managed runtime acquisition share this boundary.

The local-engine fixture suite checks no second launch while a lease is active, cancelled waiting requests, release-triggered switching, same-model sharing and shutdown with a queued request. This is not yet a FIFO multi-GGUF Compare scheduler: lineup restrictions remain until ordered scheduling, queue UI, timing/configuration evidence and actual offline acceptance are implemented. Long-lived same-model sharing can delay a different model. Existing CPU fallback behavior still needs an explicit Compare policy before benchmark use.

## Ordered managed job scheduling

Top-level managed-model jobs register in submission order across project workspaces and remain in persisted `queued` status until admitted. Admission precedes the general worker semaphore, avoiding worker exhaustion by local waiters. Completion/cancellation wakes the next eligible entry; weak references prevent completed task retention. A cancelled middle entry cannot start or let a later job overtake the active predecessor. Nested tasks continue to share the parent's model; a nested model/context switch while the runtime is leased is refused to avoid a parent-child wait cycle.

A cross-project native fixture holds a runtime lease, submits three tasks, cancels the middle one, and verifies queued states, model launch counts, terminal states and start-after-predecessor-finish ordering. This is job scheduling, not yet the complete multi-GGUF Compare feature: setup-only runtime callers still use lease admission, ordered Compare launch metadata/timing and fallback policy remain, and real offline model acceptance has not run.

## Sequential multi-GGUF Compare integration

The native and picker one-GGUF restriction is removed. Two or three installed compatible local models now use the ordered managed-job scheduler from one comparison snapshot. Offline mode remains authoritative. Compare preparation disables automatic CPU fallback and rejects reuse of an automatically-fallen-back runtime; normal non-Compare fallback behavior remains unchanged. Each local lane records runtime ID, reported backend, effective context size, preparation duration and fallback policy; the lane card displays the useful runtime details. Preparation includes readiness/wait work and is not a measured time-to-first-token.

End-to-end managed-runtime fixtures run two GGUF lanes in offline mode, verify sequential completion and shared snapshot, and cancel a three-model lineup while another lease occupies the runtime, proving no queued model is loaded. A failed-GPU fixture proves comparison policy does not launch a CPU retry. The full UI suite passes 314 tests and the production build. These simulated model/runtime files do not prove real inference or absence of external packets; actual two-model offline acceptance, complete model file/quantization/template identity, separated timing metrics and native UI acceptance remain required. Earlier entries describing the one-GGUF restriction are superseded by this increment.

## Next implementation work

1. Complete Compare journal/recovery and immutable preview binding; add project-scoped check configuration UI. The original arbitrary-command false verification is fixed in source with a native regression.
2. Immutable draft release staging and required gates, preserving all current checks; dependency advisory remedy.
3. Compare fault injection, durable journal, process-exit cleanup checks, cross-project synchronization.
4. Provider capability/conformance and real coding evaluations, extending existing login integrations using supported vendor mechanisms.
5. Shared local-resource scheduling and actual offline multiple-GGUF acceptance.
6. UX, diagnostic export, measured performance, Wayland and package/recovery qualification.

No paid/live provider turns, billing changes, release publication, or destructive Git repairs are authorized by the reference document itself. No new required hosted backend or account. Tests use isolated scratch profiles and projects.

## Real single-model offline acceptance

The opt-in `live_local_acceptance_from_explicit_models` harness accepts one to three explicit installed GGUF paths, creates an isolated profile/project and downloads nothing. On 2026-09-26 the one-model case completed on the installed Qwen2.5 0.5B Q4_K_M (491,400,032 bytes) and llama.cpp 0.4.1-dev / 18f9f7bef. A Linux user/network namespace exposed only loopback; a `strace -f -e trace=connect` trace observed ten connections to 127.0.0.1 and no nonloopback connects. The runtime reported Vulkan / RTX 5060 Ti and no CPU fallback.

The supplied addition-function explanation completed in about 1.99 seconds, including 1.63 seconds of runtime preparation. This is one inference observation, not TTFT, a coding benchmark, independent GPU-offload measurement or multi-model acceptance. Verification correctly remained `not_run`. The real test and all-target Clippy with warnings denied pass. LOC-01 remains open: only one installed GGUF was found; the two-model scheduler is currently covered by fixtures. No installed app, model or account profile was modified.

## Live subscription text/resume and Claude duplication fix

After the user offered existing subscriptions for testing, source-built `live_vendor_turn` runs used isolated profiles/projects and installed official CLI logins. Codex 0.158.0-alpha.2.1 (ChatGPT Pro, GPT-6-Luna), Claude Code 2.1.278 (Claude.ai Max, Default), Cursor 2026.09.15-d2fe57e (Auto), Antigravity ACP 1.2.1 (Gemini 3.8 Flash High) and Grok 1.0.41 (Grok 4.7) each completed an initial one-word answer and native-session resume. Cursor's CLI reported Free; paid entitlement remains unconfirmed. Unknown plan allowance remains unknown.

The real Claude run initially returned `ALPHAALPHA` twice. Its per-message streaming marker reset before `result`, allowing repeated final text. A separate per-turn emitted-text marker now suppresses that duplicate and resets on follow-up. Regression fixtures cover streamed and complete-message paths, later result-only turns, genuine repeated chunks, multiple messages and tool-then-result fallback. The real Claude recheck returned exactly `ALPHA` on both turns. The live example now fails on a wrong/duplicate answer and cancels timed-out jobs before shutdown.

All 19 adapter tests and core all-target Clippy with warnings denied pass. These live tests establish basic text and resume behavior, not full provider conformance or coding-quality acceptance. No tool action, image, quota exhaustion or auth-expiry case was exercised live in this increment. The installed app and billing settings were not changed. Codex subscription authentication was cross-checked with [official OpenAI documentation](https://learn.chatgpt.com/docs/auth).

## Full acceptance register

Initial state below is **mapping pending**, not an assertion that existing functionality is missing. Each ID needs an exact test reference and qualified result before completion. Existing successful broad suites do not automatically prove each invariant.

- **RUN-01 — Very fast hermetic task**: Terminal state observed; CLI exits successfully. Status: mapping pending.
- **RUN-02 — Engine throws before streaming**: Failure propagated; no permanent spinner. Status: mapping pending.
- **RUN-03 — Cancel before startup**: Task never executes; cancelled state persists. Status: mapping pending.
- **RUN-04 — Cancel during output**: No late event changes state to success. Status: mapping pending.
- **RUN-05 — Two sessions and duplicate events**: No cross-session completion; duplicates deduplicated. Status: mapping pending.
- **RUN-06 — Owned child ignores graceful stop**: Bounded escalation; unrelated processes untouched. Status: mapping pending.
- **CMP-01 — Crash before project mutation**: Project unchanged; operation recoverable. Status: mapping pending.
- **CMP-02 — Crash after apply before applied receipt**: Recovery reconciles actual content; no blind reapply. Status: mapping pending.
- **CMP-03 — Crash after receipt before cleanup**: Applied result preserved; cleanup retries safely. Status: mapping pending.
- **CMP-04 — Failure deleting one lane**: Remaining lane retained and reported pending. Status: mapping pending.
- **CMP-05 — Other lane refuses to stop**: Its worktree is not destructively removed. Status: mapping pending.
- **CMP-06 — Duplicate Keep or Keep/Discard race**: One logical result; no double score or mutation. Status: mapping pending.
- **CMP-07 — Project edited after preview**: Revalidate/review conflict; no stale silent overwrite. Status: mapping pending.
- **CMP-08 — Staged/unstaged changes and unsaved buffer**: Index and unrelated work preserved. Status: mapping pending.
- **CMP-09 — Binary, rename, delete, executable bit**: Result faithful; review and recovery remain usable. Status: mapping pending.
- **CMP-10 — Separate app processes target one project**: Ownership/revision conflicts stop competing writes. Status: mapping pending.
- **CMP-11 — Project A shutdown stalls**: Project B status/cancellation remain responsive. Status: mapping pending.
- **LOC-01 — Two installed GGUF models offline**: sequential two-model fixture passes. Real single-model inference passes in a loopback-only network namespace; a second installed GGUF is still needed for real two-model acceptance.
- **LOC-02 — Three-model queue cancelled mid-run**: Pending models do not start. Status: mapping pending.
- **LOC-03 — Local model exceeds safe allocation**: Clear error; UI survives; project unchanged. Status: mapping pending.
- **LOC-04 — Runtime crashes or port is occupied**: Honest state and recoverable restart path. Status: mapping pending.
- **LOC-05 — One model fails in Compare**: Other lane results and verification stay intact. Status: mapping pending.
- **PRF-01 — Model claims tests passed without execution**: engine/receipt/UI regressions pass; final native executable reproduction passes. No paid provider used.
- **PRF-02 — Configured checks pass, then source changes**: post-check edits before task completion and uncommitted-content/rename unit tests pass. Native service tests cover external edits after completion and restart; Compare tests cover acceptance refresh and explicit stale-evidence review. Continuous invalidation remains open.
- **PRF-03 — No checks configured**: native arbitrary-command reproduction returns not_run; UI displays verification not run and preserves command exit evidence.
- **PRF-04 — Required check cancelled/skipped**: typed-state unit tests prevent passing; failed/cancelled task aggregation and goal gating are implemented. Complete cancellation-boundary native matrix remains open.
- **PRF-05 — Provider self-reports an external result**: Provenance differs from locally observed evidence. Status: mapping pending.
- **PRV-01 — Expired auth or unavailable quota**: Clear status; no fabricated usage or fallback billing. Status: mapping pending.
- **PRV-02 — Malformed/out-of-order protocol data**: Bounded failure/recovery; no mixed sessions. Status: mapping pending.
- **PRV-03 — Unsupported image/tool capability**: Explicit rejection or supported alternative with consent. Status: mapping pending.
- **PRV-04 — Vendor runtime updates capabilities**: Capability record refreshes; stale affordance removed. Status: mapping pending.
- **SEC-01 — Strict sandbox unavailable**: Operation refused, not silently downgraded. Status: mapping pending.
- **SEC-02 — Approval parameters change**: Old approval cannot authorize changed operation. Status: mapping pending.
- **SEC-03 — Secret-containing logs/diagnostics**: Redaction verified; preview before export. Status: mapping pending.
- **SEC-04 — Repository text requests broader permissions**: Text cannot grant permissions. Status: mapping pending.
- **SEC-05 — Symlink/path/remote destination changes**: Actual boundary policy rechecked. Status: mapping pending.
- **UI-01 — Long transcript plus active stream**: Input latency measured and bounded. Status: mapping pending.
- **UI-02 — Event gap, duplicate event, reconnect**: Resynchronize without duplicate output/tool effects. Status: mapping pending.
- **UI-03 — 100 sequential fixture tasks**: No unexplained resource-growth trend. Status: mapping pending.
- **UI-04 — Keyboard and Wayland native session**: Focus, copy/paste, resize, cancellation usable. Status: mapping pending.
- **REL-01 — Asset upload fails**: No newly advertised incomplete public release. Status: mapping pending.
- **REL-02 — Retry same version with different binary**: Refused; published version remains immutable. Status: mapping pending.
- **REL-03 — Checksum/signature mismatch**: Install/update rejected. Status: mapping pending.
- **REL-04 — Interrupted replacement or failed first launch**: Last valid app/config recoverable. Status: mapping pending.
- **REL-05 — Fresh packaged install without dev tools**: Intended standalone functions actually work. Status: mapping pending.
- **REL-06 — Required integration test is skipped**: Release cannot be labelled fully verified. Status: mapping pending.

## Evidence boundaries

Prior baseline audit: 737 Rust tests passed across completed binaries after one isolated timing rerun; eight ignored. UI: 308 unit and 44 browser E2E tests passed. Sustained native stress: 200 completions, five cancellations, ten output floods. These are baseline measurements, not verification of subsequent edits. Full live-provider, Wayland, offline multi-model, and release-package qualification remain open.

At every checkpoint record changed commit, exact commands and results, fixture/native/live scope, unresolved risks, and the next dependency. Never mark a phase complete from a passing subset alone.
