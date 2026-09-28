# Native isolated worktrees

> **Advanced.** This is reached through the drawer's Tools › Worktrees. For the everyday workflow see the [user guide](USER_GUIDE.md), [subscriptions](SUBSCRIPTIONS.md) and [local models](LOCAL_MODELS.md).

The native engine can create a Git worktree on a separate `shadowcode/<ID>` branch
from an existing local commit or branch. Staged, unstaged and untracked changes
remain in the source checkout. The new checkout starts from the selected commit.

```sh
shadowcode --workspace /path/to/repository worktree
shadowcode --workspace /path/to/repository worktree --create
shadowcode --workspace /path/to/repository worktree --create --reference main
```

In the native desktop, open the drawer's **Tools › Worktrees** (`Ctrl+K`, *Worktrees*). The panel shows the source
project and managed checkouts, creates from a chosen local reference, and opens a
checkout through the normal trust flow. **Inspect removal** shows its exact path,
branch, commit and status before enabling **Remove clean worktree**. Dirty reviews
remain blocked; an error clears the review so a fresh inspection is required.
Actions include the displayed source project, and the engine rejects a changed
selection. The panel supports keyboard focus and light/dark/compact layouts.

The command returns the new path, branch, base commit and recovery-record ID.
Open that path through the existing project controls, or use it as `--workspace`.
Review and trust the new project before running tasks there:

```sh
shadowcode --workspace /returned/worktree/path trust
shadowcode --workspace /returned/worktree/path run "Implement and test the change"
```

Creation requires a trusted, writable repository-root workspace with no active
task or manual operation in that workspace. The current desktop/CLI project stays
selected. References resolve to one local commit before checkout; no fetch or
remote branch publication is performed. Git checkout hooks and filesystem-monitor
hooks are disabled. Git's configured checkout filters still apply, as they do for
ordinary Git checkout operations.

Private records and checkouts live under the profile's
`data/managed-worktrees/records` and `data/managed-worktrees/checkouts` directories.
A recovery record is saved before Git starts creating the checkout. An interrupted
or failed operation is recorded as `needs_attention`; an abruptly killed process
may leave `creating`. Partial files and branches are retained for inspection.
The active inventory currently permits at most 64 records. Successful removal
archives its record under `records/archive`, freeing an active slot. Git's normal worktree and branch
commands remain available for inspecting the repository metadata.

## Inspect and remove a clean checkout

```sh
shadowcode --workspace /path/to/repository worktree --inspect FULL_WORKTREE_ID
shadowcode --workspace /path/to/repository worktree --remove FULL_WORKTREE_ID --hash REVIEW_HASH
```

Inspection returns the exact path, current branch and commit, status including
ignored files, eligibility and a review hash. Removal requires that current hash
and checks the Git repository identity and registration again. It refuses dirty,
untracked or ignored files, detached HEADs, locked worktrees, active tasks/manual
operations and background processes. A reservation prevents a new managed task
or background process from starting during removal. Git performs its own final
checks; removal never uses `--force` or deletes branches. Unmerged commits remain
on the preserved branch. Source files and selection remain unchanged.

Files deliberately changed by external programs should be preserved before
removal, just as with ordinary Git operations. Missing connection files at the original managed location can be repaired as
described below. Moved paths, conflicting connections or lost administrative
metadata still require manual inspection; the recovery record remains. For a missing path whose Git
registration survives, use the reviewed rescue below.

Broader moved-checkout and damaged-metadata recovery remains part of the
[native migration gates](archive/NATIVE_MIGRATION.md).

Inventory and individual actions share the same bounded, non-following record
reader. A record whose filename, managed path or branch identity has changed is
rejected before it can be presented as a usable worktree.

## Rescue a missing checkout's committed work

```sh
shadowcode --workspace /path/to/repository worktree --recovery FULL_WORKTREE_ID
shadowcode --workspace /path/to/repository worktree --restore FULL_WORKTREE_ID --recovery-hash REVIEW_HASH
```

The review resolves the missing checkout's registered HEAD, including commits
not merged into the source project. Restoration creates a **new** managed branch
and checkout from that exact commit. It does not reconstruct missing uncommitted
files. The original branch, registration, index and recovery record remain
untouched so staged changes or a moved checkout can still be recovered manually.
No global Git prune, forced removal or branch reset is performed.

An existing path (including a symlink), locked registration, changed repository,
missing commit, unavailable registration or stale review blocks rescue. A locked
worktree may be on an unavailable device; inspect its location before unlocking.
Trust and writable/idle source requirements also apply. Open and trust the new
checkout explicitly. The original record remains in the active inventory until
its underlying checkout is repaired and reviewed for removal. Automatic repair of damaged paths remains open.

In **Tools › Worktrees**, choose **Review missing checkout** for the affected
record. The focused review shows its missing path, retained branch and exact
commit, together with the uncommitted-file limitation. **Restore in new worktree**
creates the separate checkout; **Cancel recovery** closes the review without
changes. Errors clear stale reviews. Use **Open worktree** on the newly listed
checkout to enter the usual project trust flow.

## Review and return committed changes

```sh
shadowcode --workspace /source/project worktree --review-return FULL_WORKTREE_ID
shadowcode --workspace /source/project worktree --return-changes FULL_WORKTREE_ID --return-hash REVIEW_HASH
```

The review includes the exact source/worktree branches and commits, their merge
base, and the worktree's incoming diff from that base. This is the incoming
branch diff, not a promise of a conflict-free merge. Reviews larger than the
64 KB Git-output limit must be handled through ordinary Git review tools.
Both checkouts must be attached to branches, free of tracked/untracked changes
and unfinished Git operations. Locked worktrees and already-integrated commits
are rejected. The source must be trusted and writable. Active tasks/manual
operations or background processes in either checkout block the return.

The return rechecks the review hash, then prepares `git merge --no-commit --no-ff`
in the source checkout. HEAD stays unchanged: review the staged result and commit
separately using the source project's normal Git review tools. Git preserves
nonconflicting source-branch work when histories have diverged. Conflicts remain
in the source files/index for explicit resolution; the result is
`needs_attention`, with a nonzero CLI exit status. Inspect `git status`, resolve
and commit, or run `git merge --abort` in the source project to abandon the merge.
Interrupted operations retain a `returning` recovery record explaining what was
attempted. No reset, auto-stash, automatic commit, branch deletion or checkout
removal is performed. Ignored source files cannot be overwritten by the merge.
The worktree branch and committed contents remain available throughout.

Git hooks and filesystem-monitor hooks are disabled for these operations; Git's
configured merge drivers and checkout filters still apply. Keep external Git or
filesystem writers idle during review and return, as with ordinary Git merges.
After committing or aborting, the record's last-operation status is historical;
future actions inspect the actual Git state again.

In **Tools › Worktrees**, choose **Review return**. The focused review shows
both branches and commits, the source path, and a keyboard-scrollable incoming
diff. **Keep changes isolated** cancels the review. **Prepare merge in source**
requires that exact review and creates no commit. The resulting card explains
whether the merge is pending or needs attention; **Open source project** takes
you to the checkout where you can review, resolve conflicts and commit. A failed
or stale action clears the review and displays an error rather than a success
notice.

## Copy reviewed uncommitted changes

```sh
shadowcode --workspace /source/project worktree --review-changes
shadowcode --workspace /source/project worktree --copy-changes --copy-hash REVIEW_HASH
```

The review contains separate staged and unstaged patches, the exact base commit,
and untracked file names, sizes, hashes and permissions. Copying rechecks the
review and reads a stable snapshot before creating a new branch at that commit.
It reproduces staged versus unstaged edits, binary patches, intent-to-add entries,
and Git-visible regular untracked files, including their ordinary permissions.
It verifies the destination's patches and untracked contents against the review.
The original checkout, index and files are not stashed, reset or removed.
Open and trust the new checkout explicitly before using it for tasks.

Ignored files and non-Git filesystem objects such as FIFOs are not copied.
Git-visible untracked symlinks and changed submodules are rejected; preserve those
separately. Existing Git operations/conflicts must be resolved first. Each patch
and Git listing must fit the 64 KB command-output limit; at most 256 untracked
files, 4 MB per file and 32 MB total are supported. The source must be trusted,
writable and idle, with no managed background process. The destination is
reserved while patches/files are applied. Keep external writers idle during
inspection, as with ordinary Git snapshot operations.

A stale hash fails before creation. If checkout/application is interrupted or
verification fails, the source remains intact and the partial destination stays
recorded for inspection; no automatic deletion hides evidence. Temporary patches
use private files and are removed on ordinary completion/error.

In the desktop's Tools › Worktrees, choose **Review current edits**. The review
shows the source HEAD, separate staged and unstaged patches, untracked file
names and sizes, and any intent-to-add entries. **Copy into new worktree** uses
that reviewed HEAD regardless of the separate creation reference field. The
result appears in the inventory; open and trust it before running tasks.
Cancellation leaves the source untouched, and rejected reviews must be refreshed.

Git requires a configured identity even for a merge prepared without committing.
If return reports an identity error, configure your own `user.name` and
`user.email` in the source repository, then review and retry. ShadowCode does not
invent an identity or modify your Git identity settings.


## Repair missing Git connections

Use **Tools › Worktrees › Review connection repair** when the original managed
checkout still exists but its `.git` file or the repository's corresponding
`gitdir` connection is missing. The review identifies the checkout, retained
branch/commit and which connection is missing. **Restore Git connection** writes
only missing connection files; it does not rebuild an index, reset files, change
branches or recreate the checkout.

```sh
shadowcode --workspace /source/project worktree --review-repair WORKTREE_ID
shadowcode --workspace /source/project worktree --repair WORKTREE_ID --repair-hash REVIEW_HASH
```

The source must be trusted, writable and idle. Active source/worktree tasks and
managed background processes block repair. Keep external Git operations and
filesystem writers idle during review and repair. A changed review is rejected;
new files created by another writer are never overwritten. The operation retains
a private journal under `managed-worktrees/records/repairs` and records interrupted
or failed repairs for inspection instead of rolling back or deleting evidence.

This repair requires the original real directory, retained managed branch,
common-directory identity and existing regular index. Locked worktrees,
symlinks, connections pointing elsewhere, moved checkouts and missing indexes
are refused. It cannot reconstruct deleted uncommitted data. Preserve those
cases for manual recovery; use the separate missing-checkout rescue only when its
review matches the situation. No Git configuration or identity is changed.
