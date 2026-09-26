<!-- User-supplied design and acceptance reference. Evidence must be rechecked against the implementation. -->

# ShadowCode — Engineering Upgrade and Acceptance Specification

Repository: Shadowfetchapps/ShadowCode  
Review date: September 26, 2026  
Source reference examined: `e371baaa4c690527173a322f7e0e1cfae4db329a`

## Review scope and evidence qualification

This specification follows a source and CI review, not a local execution of the native application or a live authentication test of paid providers. GitHub returned inconsistent snapshots during retrieval. Recheck every observation against the actual checkout and current CI before implementing a fix. Do not delete working functionality because an earlier report describes a different revision. No dependency-install failure or particular CLI timeout root cause should be assumed.

The most recently retrieved job summary shows UI and Rust checks passing and the headless native CLI step failing; subsequent integration checks were skipped. The exact cause requires reproduction. The Compare excerpts were consistent across consecutive reads with blob SHA `58e342beb5263e31255af60b18dc1c858491c21a`. The final release-workflow excerpt had blob SHA `9079d8ea66390a64c7c24d469b251735408f487d`.

The source-level observations below are narrow, not claims of observed data loss or a demonstrated sandbox escape:

- In `native/core/src/compare.rs`, `keep` applies the selected checkout to the user's working tree, updates an in-memory record, attempts lane cleanup, then persists the record. `discard` also performs cleanup before its final persistence. The filesystem/database transition deserves explicit crash-recovery tests.
- `keep` records an error returned by `stop_lanes` as a note and continues toward cleanup. Lower-level disposal might have additional protections; inspect it. Establish and test the invariant at every destructive boundary rather than assuming that cancellation was acknowledged.
- Compare's `get`, `list`, `keep`, `cancel`, and other handlers acquire a shared `LOCK`; handlers can await Git or lane shutdown while holding it. A cancellation wait is bounded to 60 seconds per active lane. Measure cross-workspace blocking before changing synchronization.
- `resolve_models` requires two or three distinct models, but rejects more than one ID beginning `local:gguf:`. This prevents comparing two downloaded GGUF models, even though they could run sequentially. It does not establish that every other offline provider combination is impossible.
- `apply_checkout` already uses `git apply --check`, generates a binary-capable patch, and applies to the working tree rather than the user's index. Preserve those safeguards and semantics.
- The release workflow already runs source, behavior, desktop, runtime, packaged-application, checksum, and installer checks before its final publication step. Do not replace that with weaker CI. Its final step creates a public release and then uploads artifacts with `--clobber`; strengthen atomic publication and immutable release assets.

All proposed performance limits below are acceptance targets, not measurements of current performance.

---

# COPY-READY ENGINEERING PROMPT

You are the lead engineer responsible for upgrading ShadowCode into a dependable, polished, standalone native coding application.

Work in the existing Shadowfetchapps/ShadowCode repository. This is a production hardening and carefully scoped feature-extension task, not permission to rebuild the product.

The desired experience is simple: open a project, select a supported subscription-backed coding runtime or a compatible local model, give it a task, see what is happening, review the actual changes and test evidence, and keep or discard the result without risking unrelated work. The user should not have to operate a separate server to use the application's managed local runtime.

## Non-negotiable constraints

1. Preserve the existing native architecture, editor, UI framework, provider adapters, persisted sessions, managed runtimes, worktrees, security boundaries, CLI/TUI, MCP interfaces, and useful tests. Read the actual manifests before naming their locations or proposing replacements.
2. Do not add a required hosted backend, cloud database, ShadowCode account, marketplace, orchestration framework, or externally managed inference service. Optional external integrations must remain optional.
3. Keep a single understandable provider/model picker. Do not turn every capability into another permanent dashboard or navigation destination.
4. A subscription runtime, a direct paid API, and a local model are different execution and billing routes. Never silently substitute one for another. Never fabricate quota, cost, model capability, test success, or sandbox enforcement.
5. Preserve existing working Antigravity integration and other provider-specific startup/authentication behavior. Investigate actual capability and protocol support instead of treating a functioning integration as a placeholder.
6. Do not erase or overwrite unrelated source changes, editor buffers, staged content, user branches, credentials, or histories. Do not use destructive Git reset/clean operations as an automatic repair.
7. Do not weaken CI, remove failing tests, increase timeouts blindly, introduce fake success states, or use mocks as proof that live authentication works.
8. Work on a dedicated branch with focused commits. Do not force-push, publish a release, change billing, or initiate paid/live provider requests without explicit authorization. Routine implementation decisions do not need repeated clarification.
9. Where a requested feature already exists, strengthen its acceptance tests and integration rather than creating a duplicate implementation.

## Phase 0 — Establish the real baseline

Inspect the current commit, working-tree status, package manifests, toolchain pins, lockfiles, native/UI boundaries, IPC contracts, session store, compare/worktree implementation, provider adapters, local inference manager, security documentation, and all relevant workflows.

Read `native/core/src/compare.rs`, `.github/workflows/native.yml`, `.github/workflows/release.yml`, `SECURITY.md`, and the implementation report where those files exist. Locate the actual application and test entry points from the current tree. Specifically inspect `scripts/test-native-cli.mjs` if present; do not invent an obsolete CLI invocation.

Create or extend an audit document containing: observed behavior, source location, reproduction, severity, proposed correction, regression test, and final result. Label entries CONFIRMED, RISK TO TEST, PROPOSED UPGRADE, ALREADY IMPLEMENTED, or NOT VERIFIED.

Reproduce existing checks before edits where feasible. Record failures separately from environment limitations. Do not attribute failures to the lockfile or completion-event transport without evidence. Check the latest failing run for the exact checkout being tested.

## Phase 1 — Make native task completion and cancellation reliable

Reproduce the headless native CLI failure. Inspect argument parsing, profile/config loading, deterministic test provider selection, engine dispatch, event subscription, session/run identifiers, terminal state persistence, error propagation, and process exit.

The application must provide these invariants:

- Every accepted task reaches exactly one logical terminal state: completed, failed, or cancelled. Terminal events may be redelivered, but consumers must deduplicate them by stable identity.
- Early completion cannot be lost because a subscriber was installed late. Choose buffering, durable state lookup, subscribe-before-dispatch, or an equivalent remedy only after establishing the actual failure mechanism.
- A cancelled task cannot later become successful when buffered output arrives.
- Events from a different session or attempt cannot complete the current task.
- The CLI returns a meaningful exit code, flushes its final report, and terminates without leaked application-owned subprocesses.
- A runtime deadline produces a persisted failure and bounded cleanup, not an indefinitely spinning UI.

Make the basic smoke test hermetic: no real subscription, credentials, model download, internet access, or desktop display. Separate compilation from execution deadlines. Preserve the real integration suites and ensure formerly skipped checks actually execute successfully before claiming the pipeline is healthy.

Add deterministic coverage for rapid completion, delayed completion, worker exception, malformed output, cancellation before startup, cancellation during streaming, SIGINT, repeated tasks, parallel sessions, and child processes that ignore the first termination signal.

## Phase 2 — Make Compare keep/discard crash-recoverable

Inspect `keep`, `discard`, `apply_checkout`, `commit_checkout`, `remove_lanes`, persistence, and worktree disposal together. Keep the existing source-workspace reservation and Git safety checks.

Introduce an explicit durable operation journal using the existing storage architecture. A database transaction alone does not make Git/filesystem operations atomic. Record intent before external mutations, record the applied outcome before destructive cleanup, and reconcile uncertain transitions on restart.

The operation record should identify the transaction, comparison, selected lane, project, expected branch/HEAD, relevant index state, relevant working-tree fingerprints, lane result commit or equivalent immutable result, planned changed paths, recoverable preimages, phase, and cleanup progress. Use redaction and bounded retention rather than copying secrets into diagnostics.

Suggested internal phases are prepared, applying, applied, cleanup_pending, complete, and needs_review. Adapt names to existing public schemas and support migration; do not break existing sessions to force these exact labels.

Required sequence:

1. Acquire the appropriate project mutation reservation and validate the current record revision.
2. Refresh the selected lane's actual files and proof state. Reject or re-review a changed result instead of applying stale preview data.
3. Prepare a durable recovery record and recoverable material before touching the source working tree.
4. Perform existing preflight validation and apply the selected result while preserving the user's index and unrelated modifications.
5. Persist the applied outcome durably before removing recoverable lane material.
6. Confirm other lane jobs and their managed descendants have stopped before deleting their worktrees.
7. Perform idempotent cleanup. Persist each useful checkpoint and report cleanup_pending honestly when anything remains.
8. Mark the operation complete only after its logical result is durable; cleanup failure must not cause the result to be applied twice.

If the app crashes after filesystem application but before the applied record is committed, recovery must inspect recorded preimages/postimages and actual project state. Do not blindly reapply, blindly revert, or silently declare success. Ambiguous user-edited state requires a clear recovery review.

Preserve the current working-tree-only application behavior. Do not start committing or staging on the user's branch as an incidental side effect. Preserve binary changes, deletes, renames, executable bits, path quoting, and unrelated staged/unstaged changes. Handle dirty editor buffers separately: an unsaved buffer is not the same as the file on disk.

Inject failures before and after every durable state transition, patch application, and worktree cleanup. Cover disk-full/write refusal, concurrent project edits, process crash, missing worktree, restart, and duplicate Keep clicks. Confirm scoreboard updates cannot be counted twice.

## Phase 3 — Correct cancellation and synchronization boundaries

Treat cancellation request, cancellation acknowledgement, and process exit as separate states.

In `keep`, do not merely append a stop error and continue destructive cleanup of that lane. Inspect lower-level disposal protections, add caller-level checks, and test both. Keep the already-applied winner recoverable while unresolved lanes remain explicitly pending cleanup.

Replace unnecessary global Compare serialization only after proving the lock scope. Prefer project-scoped mutation coordination and comparison-scoped state synchronization. Preserve cross-process ownership checks and revision conflicts; an in-process mutex is not cross-process protection.

Avoid holding the global metadata lock while awaiting a provider, Git subprocess, or long shutdown. Use a documented lock order. Preserve one writer per project and prevent concurrent Keep/Discard from racing. Cancellation and status inspection for project B must remain responsive when project A has a slow shutdown.

Track process trees, not just the first child PID. Use platform-appropriate termination and a bounded escalation policy. Never kill an unrelated process or assume a recycled PID is still owned by the application.

## Phase 4 — Add sequential local GGUF comparison

Remove the blanket one-GGUF restriction only after adding explicit scheduling. The new feature is: compare two or three installed compatible GGUF models on the same task and project snapshot with the network disabled.

Use one application-managed local inference slot by default. Queue Model B while Model A is loading/running; release or reuse resources safely before loading the next model. Do not load multiple large models simultaneously and hope memory is sufficient. Other application jobs must share the same local-resource admission control.

Freeze equivalent task instructions, project snapshot, tool permissions, web policy, and verification commands. Record actual model identifier, file identity, quantization, template, context configuration, and relevant sampling settings. Do not pretend different tokenizers have identical token budgets or that hardware-dependent estimates are guarantees.

Show queued, loading, generating, running_tool, verifying, stopping, and ready states. Report model-load time separately from time to first generated token, generation time, verification time, and total wall time.

Cancellation must remove queued work and stop active work without accidentally loading the next model. A failed model must not destroy other lane results. An out-of-memory error must leave the UI responsive and the project intact. Offer an explicit lower-context or CPU fallback decision instead of silently changing the benchmark conditions.

Prove the all-local mode makes no external network requests. Do not auto-download missing models while claiming a fully offline run. Preserve per-lane evidence and let the user choose; any recommended winner must explain its evidence and limitations.

## Phase 5 — Upgrade verification from prose to typed evidence

Extend the existing task-proof implementation instead of replacing it with another subsystem.

A verification receipt should include task ID, attempt ID, tool invocation ID, workspace identity and relevant content fingerprint, verification kind, redacted command/arguments, working directory, start/end timestamps, exit code when available, termination reason, output references, and evidence provenance. Include parser/version details when extracting structured test counts.

Use explicit states: not_run, running, passed, failed, cancelled, skipped, and stale. Add timeout as a termination reason. Execution completion and verification success are independent fields.

A command name that resembles a test command is not by itself proof that tests ran. A model saying “all tests pass” is not equivalent to a recorded process result. Distinguish locally observed evidence, provider-reported evidence, and unsupported/unverified claims.

Invalidate results when relevant source/configuration changes after verification, including uncommitted changes. Git HEAD alone is insufficient. Avoid hashing huge generated directories on every token; use the existing file-change/indexing architecture with a conservative final pre-acceptance check.

Keep test-scope language precise. “Configured checks passed” does not mean the entire application is correct. A no-check task should display “Finished — verification not run,” not a green verified badge. Cancelled or skipped required checks must never count as passed.

Make each result clickable to its exact command, output, files, and attempt. Re-running verification should not require re-running a paid generation task.

## Phase 6 — Harden existing provider adapters

Keep supported vendor-native session, authentication, approval, cancellation, and usage mechanisms. Read the installed provider version and current official protocol documentation. Do not assume one protocol or identical capabilities across all providers.

Build a common adapter conformance suite covering startup, readiness, capability discovery, streaming, tool events, permission prompts, cancellation, session continuation, expired authentication, rate limits, process death, malformed protocol data, and shutdown.

Track protocol version, detected capabilities, last check time, and supported/unsupported operations. Transport stdout must remain parseable; diagnostics must not corrupt protocol messages. Bound message size, request queues, and retained output.

Expose support honestly for image input, tool calling, web access, model selection, continuation, and quota. Unsupported image attachments must be rejected with an explanation rather than silently discarded. Do not route them through a different paid/cloud provider without explicit consent.

Keep subscription usage distinct from pay-per-token API estimates. Display provider-reported limits with their observation time and reset information when actually available. Use “Provider does not expose remaining usage” or “Usage unavailable” when necessary. Never derive remaining subscription quota from an invented token allowance.

Use redacted fixtures for normal CI. Real authentication and paid-turn tests are opt-in and reported separately. A fixture passing must not be labelled a successful live provider session.

## Phase 7 — Local runtime, context, and security UX

Provide one in-app diagnostic path for model file validity, runtime identity, backend actually in use, driver/backend compatibility, measured allocation failures, context configuration, available memory, and owned process state. Reuse an existing diagnostics screen if present.

Treat memory-fit estimates as estimates. Probe safely and report the backend actually running. A GPU-capable build is not proof that inference is executing on the GPU. Handle ports in use, runtime crash, partial model download, failed integrity checks, and interrupted startup without leaving stale running badges.

For managed runtimes, bind internal services to the intended local interface, protect management endpoints, and clean up application-owned resources on quit. Do not treat every local process as trusted merely because it is on loopback. Preserve existing credential isolation.

Add an inspectable context panel: included files/ranges, inclusion reason, token estimate, truncation/compaction notices, and excluded sensitive material. Respect repository/application ignore rules. Test rename/delete/branch-switch invalidation. Keep local search/indexing local unless the user explicitly chooses a remote service; do not add a mandatory vector database.

Show the enforcement actually applied to each operation: enforced sandbox, best-effort sandbox, unsandboxed trusted runtime, or remote execution. Do not imply ShadowCode's local tool sandbox contains a vendor runtime's independent tools.

Where strict enforcement is selected, fail closed when the required boundary is unavailable. Scope approval to the actual operation, project, paths, working directory, destination, and relevant parameters; changes invalidate stale approval. Background processes, hooks, MCP tools, downloads, and remote tools need explicit policy and truthful labels, not a decorative shield.

Test secret redaction in tool receipts, logs, prompts, exported diagnostics, and crash reports. Treat repository text and tool output as untrusted instructions rather than permission grants. Never export credentials by default.

## Phase 8 — Simplify the daily interface and improve performance

Keep the main experience focused on project/files, editor, and agent conversation. Put changes, verification evidence, and terminal behind useful contextual tabs or panels. Do not make graphs, agent org charts, cost charts, or settings dominate routine work.

The primary controls should expose the selected model plus execution/billing route, task mode, real web state, actual permission policy, and Run/Stop. Show a compact meaningful task state rather than an indefinite spinner. Make queued, loading, waiting_for_permission, generating, executing, verifying, stopping, failed, and complete distinguishable.

Protect unsaved buffers when agent edits arrive. Show a conflict/merge choice instead of silently overwriting. Review hunks must be tied to the current revision and revalidated before application.

Use incremental streamed events with session/run/sequence identity and bounded reducers. Avoid repeatedly sending or reparsing the whole transcript on every token. Support resynchronization after event gaps. Virtualize long histories and large diffs; load old output on demand.

Performance acceptance targets, measured on documented hardware and fixture sizes:

- Input response remains below 100 ms at the 95th percentile during a representative long streamed task.
- Coalesce text updates to roughly 20 visible updates per second while allowing urgent cancellation/permission state through immediately.
- A 10,000-message history loads in pages and does not require every message to remain mounted in the DOM.
- Repeated completed/cancelled runs do not produce unbounded memory, file-descriptor, process, or event-listener growth.
- Unrelated project status and cancellation remain responsive during slow Compare shutdown in another project.

Report baseline and after measurements; these are targets, not claims about current performance. Test keyboard-only operation, focus restoration, copy/paste, readable errors, resize, high-DPI scaling, and light/dark themes. Include actual Linux Wayland acceptance, not just headless X11 screenshots. Add a redacted diagnostic export the user can preview before sharing.

## Phase 9 — Strengthen release publication without discarding existing gates

Preserve the current source, CLI, stress, terminal, MCP, native-window, runtime, packaging, checksum, and installer tests. Reconcile package-manager and toolchain configuration against the current checkout; do not create duplicate lockfiles or switch managers as an unrelated cleanup.

Build from an immutable commit. Record version, commit, target architecture, dependency/runtime source identities, checksums, and verification result in the release manifest. Validate actual package contents, not only development builds.

Stage release assets on a draft. Verify that the full required asset set and checksums are present before publication. Do not overwrite already-published versioned artifacts using --clobber as normal release practice. A failed upload must leave a draft or an explicitly failed attempt, not a partially complete advertised release. Make retry idempotent and fail on content mismatch under the same version.

Audit signing and update support actually present in the repository. Where automatic updates are shipped, require authenticated update metadata and signature verification; do not confuse an unsigned checksum with publisher authentication. Preserve user configuration and prior valid runtime until replacement is validated. Test interruption, corrupted download, failed first launch, and rollback without destructive database downgrade.

Publish support claims only for platforms actually built and exercised. A Windows/macOS compile result is not equivalent to hands-on acceptance. Keep Linux primary; do not claim full platform parity based on mock tests.

## Required regression matrix

Implement these through existing test frameworks where possible. Each item must have a test, an existing-test reference, or an explicit not-verified reason.

| ID | Scenario | Required result |
|---|---|---|
| RUN-01 | Very fast hermetic task | Terminal state observed; CLI exits successfully |
| RUN-02 | Engine throws before streaming | Failure propagated; no permanent spinner |
| RUN-03 | Cancel before startup | Task never executes; cancelled state persists |
| RUN-04 | Cancel during output | No late event changes state to success |
| RUN-05 | Two sessions and duplicate events | No cross-session completion; duplicates deduplicated |
| RUN-06 | Owned child ignores graceful stop | Bounded escalation; unrelated processes untouched |
| CMP-01 | Crash before project mutation | Project unchanged; operation recoverable |
| CMP-02 | Crash after apply before applied receipt | Recovery reconciles actual content; no blind reapply |
| CMP-03 | Crash after receipt before cleanup | Applied result preserved; cleanup retries safely |
| CMP-04 | Failure deleting one lane | Remaining lane retained and reported pending |
| CMP-05 | Other lane refuses to stop | Its worktree is not destructively removed |
| CMP-06 | Duplicate Keep or Keep/Discard race | One logical result; no double score or mutation |
| CMP-07 | Project edited after preview | Revalidate/review conflict; no stale silent overwrite |
| CMP-08 | Staged/unstaged changes and unsaved buffer | Index and unrelated work preserved |
| CMP-09 | Binary, rename, delete, executable bit | Result faithful; review and recovery remain usable |
| CMP-10 | Separate app processes target one project | Ownership/revision conflicts stop competing writes |
| CMP-11 | Project A shutdown stalls | Project B status/cancellation remain responsive |
| LOC-01 | Two installed GGUF models offline | Sequential runs complete without network access |
| LOC-02 | Three-model queue cancelled mid-run | Pending models do not start |
| LOC-03 | Local model exceeds safe allocation | Clear error; UI survives; project unchanged |
| LOC-04 | Runtime crashes or port is occupied | Honest state and recoverable restart path |
| LOC-05 | One model fails in Compare | Other lane results and verification stay intact |
| PRF-01 | Model claims tests passed without execution | UI does not mark verified |
| PRF-02 | Configured checks pass, then source changes | Receipt becomes stale |
| PRF-03 | No checks configured | Completed but verification not run |
| PRF-04 | Required check cancelled/skipped | Overall verification not passed |
| PRF-05 | Provider self-reports an external result | Provenance differs from locally observed evidence |
| PRV-01 | Expired auth or unavailable quota | Clear status; no fabricated usage or fallback billing |
| PRV-02 | Malformed/out-of-order protocol data | Bounded failure/recovery; no mixed sessions |
| PRV-03 | Unsupported image/tool capability | Explicit rejection or supported alternative with consent |
| PRV-04 | Vendor runtime updates capabilities | Capability record refreshes; stale affordance removed |
| SEC-01 | Strict sandbox unavailable | Operation refused, not silently downgraded |
| SEC-02 | Approval parameters change | Old approval cannot authorize changed operation |
| SEC-03 | Secret-containing logs/diagnostics | Redaction verified; preview before export |
| SEC-04 | Repository text requests broader permissions | Text cannot grant permissions |
| SEC-05 | Symlink/path/remote destination changes | Actual boundary policy rechecked |
| UI-01 | Long transcript plus active stream | Input latency measured and bounded |
| UI-02 | Event gap, duplicate event, reconnect | Resynchronize without duplicate output/tool effects |
| UI-03 | 100 sequential fixture tasks | No unexplained resource-growth trend |
| UI-04 | Keyboard and Wayland native session | Focus, copy/paste, resize, cancellation usable |
| REL-01 | Asset upload fails | No newly advertised incomplete public release |
| REL-02 | Retry same version with different binary | Refused; published version remains immutable |
| REL-03 | Checksum/signature mismatch | Install/update rejected |
| REL-04 | Interrupted replacement or failed first launch | Last valid app/config recoverable |
| REL-05 | Fresh packaged install without dev tools | Intended standalone functions actually work |
| REL-06 | Required integration test is skipped | Release cannot be labelled fully verified |

## Final deliverables and completion rule

Deliver the code, migrations, regression tests, updated documentation, and measured before/after performance evidence. Produce a final report with exact commit, changed files, defects fixed, features strengthened, commands executed, test counts/results, environment limitations, live-provider status, packaged-application status, and remaining risks.

Separate implemented, tested in fixtures, tested in the native application, and tested against a live provider. Include recovery demonstrations for Compare and a fully offline two-GGUF comparison demonstration when the required models/hardware are available; otherwise identify the precise untested dependency without inventing evidence.

Complete each phase's implementation and tests before declaring it done. Do not substitute a collection of TODOs or a polished summary for working code. Do not claim perfection or universal safety. The intended standard is a smaller-feeling application whose execution, review, recovery, and release behavior are supported by repeatable evidence.

---

## Source manifest for rechecking the review

These are review references, not instructions to trust an old snapshot over the current checkout.

```text
https://github.com/Shadowfetchapps/ShadowCode/blob/e371baaa4c690527173a322f7e0e1cfae4db329a/native/core/src/compare.rs
  resolve_models / remove_lanes / stop_lanes: retrieved source lines 640–815
  get / list / commit_checkout / apply_checkout: retrieved source lines 900–1130
  keep / discard / cancel / concurrency tests: retrieved source lines 1131–1390

https://github.com/Shadowfetchapps/ShadowCode/blob/e371baaa4c690527173a322f7e0e1cfae4db329a/.github/workflows/release.yml
  checked native build and final publication sequence

https://github.com/Shadowfetchapps/ShadowCode/actions/runs/36210288819
  most recent retrieved summary: UI/Rust passes; headless CLI fails; later checks skipped

https://developers.openai.com/codex/app-server
https://code.claude.com/docs/en/cli-reference
https://v2.tauri.app/plugin/updater/
  official integration/update references; verify installed/current versions
```
