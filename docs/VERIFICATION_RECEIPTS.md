# Recorded checks and verification receipts

Task execution and verification are independent. A finished model turn or successful shell process does not prove task acceptance. The native engine extends the existing `verification.summary` event with versioned receipts; it does not create a second task store.

## What counts

A command is a configured check only when it is launched through the existing explicit test-task action, exactly matches a command discovered from project manifests in the project root, or exactly matches a user-configured `verification.commands` entry in application configuration. Execution still uses the existing trust, shell approval, sandbox, and lifecycle-hook path. Merely containing `test`, `lint`, or a runner name is not sufficient. Options and subdirectory commands that do not exactly match remain ordinary execution evidence; they can be run explicitly as checks through the test-task action.

The optional application configuration field is:

```yaml
verification:
  commands:
    - "cargo test --offline"
```

These entries are application-level root commands. Repository configuration overlays cannot grant this setting. They do not start a process or grant shell approval. The project-inspection path identifies exact manifest commands; this is not a claim that a package's `test` script truly exercises every requirement.

A passing status means **the configured checks passed**, not “the application is correct” or “all tests in the repository passed.” No test count is inferred or parsed. Custom checks may validate other acceptance conditions.

## Receipt and state

Each receipt records schema version, task and attempt identity, tool-call identity, stable check identity, command, working directory, workspace, timestamps, exit status, termination reason, provenance, scope, content fingerprint, and a reference to the exact `tool.completed` event. Persisted command text is redacted. Check identity is computed before redaction so two commands with different hidden arguments cannot silently supersede one another.

States are `not_run`, `running`, `passed`, `failed`, `cancelled`, `skipped`, and `stale`. Timeout is a termination reason and cannot pass. Current receipts are emitted at completion; the existing task/tool stream provides running state. Missing/incomplete fingerprint evidence is skipped rather than green.

Only the newest receipt for the same command identity and working directory supersedes its previous result. A different passing command cannot erase a failed check. Goal milestones requiring verification require the shared `passed` status. Compare counts typed check receipts rather than arbitrary commands, and reports stale/incomplete checks separately. The UI retains general command exit results without calling them checks; receipt details expose provenance, attempt, fingerprint, and recorded output.

## Freshness and scope

Fingerprints include non-ignored file names, content, and executable modes, including uncommitted changes. Generated directories `.git`, `node_modules`, `target`, `dist`, `build`, `.venv`, and `__pycache__` are outside the scope. Reading is confined and nonblocking; unsupported/nonregular files, oversized inputs, traversal errors, and budget exhaustion prevent a fingerprint. Limits are 100,000 entries, 128 MB total, the existing 4 MB per-file read bound, and a five-second scan budget.

The engine captures before/after check state and rechecks passing receipts before publishing its final summary, after completion hooks. Changed content makes evidence stale. Hashing occurs around checks and final validation, not every streamed token. A receipt is evidence of that observed snapshot; it is not continuous filesystem monitoring.

Historical results are reassessed through `GET /api/jobs/{id}/verification` without rewriting recorded receipts. Visible task summaries request an assessment on mount, focus, and explicit refresh. Pending or unavailable assessments cannot reuse the old green status. External edits and service restart are covered by a native service regression. This is point-in-time validation, not continuous watching.

Compare reassesses completed lanes and repeats validation before Keep. A stale or incomplete configured check requires explicit Keep-without-current-checks review. Keep reserves the selected lane against new application-owned jobs during validation/capture; checks recorded at acceptance remain distinguishable after cleanup. This reservation does not prove cross-process or external-editor exclusion.

Remaining integration work: efficiently share change-index fingerprints, continuous invalidation, project-scoped check configuration UI, immutable preview binding and unsaved-buffer protection, transactional Compare recovery, and broader packaged/live-provider qualification. Existing explicit test tasks already rerun a command without making a paid model request.

## Compatibility and evidence

Finalization preserves task-specific command receipts even if a token limit, provider error, cancellation or interrupted-process recovery prevents a final assessment. An existing assessment is retained, including stale states; only newer receipt events are appended. The aggregate is unverified, with `final_assessment=not_completed` (or `interrupted` on recovery). Individual states and output references still describe what actually ran. The UI reports that final verification did not finish instead of claiming a previously successful check failed. Historical reassessment cannot promote a failed, cancelled or interrupted task to a verified claim or red-green success.

This fallback reads only typed verification events and performs no workspace hashing, commands or automatic replay. It does not establish current file freshness. Startup tests seed persisted active jobs and reopen twice; they are recovery-state coverage, not an OS-crash injection demonstration.

Receipt events and summary fields are additive to existing persisted events. Legacy command-only rows remain readable but cannot establish verification. Vendor-owned verification retains its distinct provenance and is not promoted into local evidence. A cancelled or failed task cannot publish an aggregate verified result.

Regression coverage includes the original `printf test` false-positive, no-command model claims, explicit configuration, edits after a check, exact-command recovery, unrelated failures, cancellation/skipped/stale states, missing fingerprints, uncommitted content, rename behavior, ignored scope, goal acceptance, and UI labels. See `engine_tasks`, `goals`, `verification::tests`, `autonomy::tests`, and `ui/src/lib/activity.test.ts` for executable cases.
