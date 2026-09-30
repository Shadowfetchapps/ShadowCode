# Your data: upgrades, backups, restore, repair and reset

ShadowCode keeps your settings, conversations and history on your computer.
This page says where, what happens to them when you upgrade, and how to back
them up, restore them, repair them and start over. Everything here is in
**Settings › Your data**, and on the command line.

## Where your data is

| Folder | Holds |
| --- | --- |
| `~/.config/shadow-agent` | `config.yaml` (settings, [every key](../config.example.yaml)), `secrets.env` (API keys, mode 600), `remote.json` (remote access and paired devices) |
| `~/.local/state/shadow-agent` | `shadow-agent.db` (conversations, tasks, jobs, events, goals, automations, comparisons, usage), automatic copies made before upgrades, small caches |
| `~/.local/share/shadow-agent` | backups, managed worktrees, downloaded models (local, voice, code search), webview storage |

`XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_DATA_HOME` move them when set.
`shadowcode --profile DIR` uses `DIR/config`, `DIR/state` and `DIR/data`
instead, for a separate profile. Your own agent definitions live in
`~/.config/shadowcode/agents`, next to the profile.

**The folder names are permanent.** They are still called `shadow-agent`, the
app's name before ShadowCode, and they keep that name in every 1.x version so
that every upgrade finds your data. Don't rename them.

## Upgrades

- **Every version since 0.28 upgrades in place.** When a new version opens an
  older database it first copies it to
  `shadow-agent.pre-native-<id>.sqlite` in the state folder (only your account
  can read it), then upgrades it in one step that either finishes or changes
  nothing. Settings from older versions are read as they are; keys a version
  no longer uses are kept and ignored.
- **Tested for every release.** The test suite opens a profile written by each
  published release from 0.28.0 on (`native/core/tests/fixtures/upgrade`) and
  checks that every conversation, task, job, goal, automation, comparison,
  unsaved editor draft, usage record, setting and API key is still there.
- **Going back to an older version.** An older version refuses a database a
  newer version has changed, and says which version to use (the app shows it
  in a message and closes); it changes nothing. To go back, restore a backup
  made by the older version, or one of the copies made before the upgrade
  (below), with `shadowcode restore FOLDER` in a terminal: the older app
  cannot open Settings on that profile.

## Back up

**Settings › Your data › Back up now** (or `shadowcode backup`) writes a
folder named `shadowcode-backup-<date>-<time>` in
`~/.local/share/shadow-agent/backups`. **Back up to another folder…** (or
`shadowcode backup -o FOLDER`) puts it somewhere else, for example a USB
drive. The folder you choose is left as it is (it may be shared, or a link to
another disk); a missing one is created. It holds:

- a consistent copy of the database, taken while ShadowCode keeps working;
- `config.yaml`, your agent definitions and plugin records (hidden files and
  folders such as `.git` are skipped);
- `manifest.json`, with the size and SHA-256 of every file, so a damaged or
  incomplete copy is noticed before it is restored.

Files larger than 4 MB, and small files past the first 4,096, are left out;
the backup says which, and it never fails because of them.

API keys (`secrets.env`, and the keys you moved to the desktop keyring) and
remote-access pairing (`remote.json`) are left out unless you tick **Include
API keys and remote-access pairing** (or pass `--include-secrets`). If the
keyring is locked, the keys it holds are left out and the backup says so.
Anyone who gets such a backup can use your keys: keep it private. Only your
account can open the backup folder. Downloaded models, worktrees and your
project files are not included.

## Restore

Choose **Restore…** next to a backup, or **Restore from another folder…**
(`shadowcode restore FOLDER`). ShadowCode first checks the backup and shows
what it holds: the version that made it, the number of conversations, tasks,
goals and automations. It refuses a backup that is incomplete, changed or
damaged, that was made by a newer version, or whose settings don't load, and
says why.

The restore happens the next time ShadowCode starts, because the running app
has the database open. Quit what holds your data and open ShadowCode again:
Settings › Your data says which process it is (the app, `shadowcode serve`,
ShadowCode in a terminal, or an editor running `shadowcode acp`). **Quit
ShadowCode now** is offered when it is this window. Until then you can cancel
it. When it runs, ShadowCode:

1. checks the copied files again;
2. backs up what it is about to replace (`…-before-restore`);
3. puts the backup's database and settings in place; your API keys only if
   you ticked **Also restore the API keys in this backup** (restored keys are
   read from `secrets.env`: move them to the keyring again in Settings ›
   Accounts if you like); and remote access with its paired devices only if
   you ticked **Also restore remote access and paired devices**;
4. upgrades the database if the backup came from an older version.

Remote access comes back switched off. Devices you removed since the backup
was made can connect again, so check the paired devices in Settings › Remote
access before you turn it on.

`shadowcode restore FOLDER --yes` restores right away when ShadowCode is not
running; without `--yes` it only checks the backup. Add `--include-secrets`
for the API keys and `--include-remote` for remote access. **Copies made
before upgrades** can be restored the same way; they bring back your history
as it was, and leave your settings as they are.

## Check and repair

**Check and repair** (or `shadowcode doctor --repair`) backs up the database
first (`…-before-repair`), then:

- checks the whole database for damage;
- counts records that point to ones that no longer exist (they are kept);
- rebuilds the search indexes and statistics;
- merges the write-ahead log into the database;
- moves caches ShadowCode can download again (the OpenRouter model list) into
  the repair backup, and forgets cached sign-in states.

Nothing you made is removed. If it finds damage, it says so: restore a
backup.

## Start over

**Reset ShadowCode…** (or `shadowcode reset --yes`) moves your settings, API
keys, conversations and history into folders named `<folder>.reset-<date>-<time>`
next to the three folders above, so ShadowCode starts as if it was just
installed. Nothing is deleted. Backups, worktrees and downloaded models stay
where they are, also when the `XDG_*` variables put the three folders in one
place. Like a restore, it happens the next time ShadowCode starts
(right away from the command line when ShadowCode is not running), and you
can cancel it until then. To undo a reset, quit ShadowCode and move the files
back.

## For developers

- Routes: `/api/data…` in [API_CONTRACT.md](API_CONTRACT.md#your-data-backup-restore-repair-reset),
  refused over remote access. Code: `native/core/src/data.rs`.
- A scheduled restore or reset is a marker (`pending-data-operation.json`) and,
  for a restore, the checked files (`pending-restore/`) in the state folder.
  The engine finishes it when it opens the profile, holding the profile lock
  and before it opens the database. The outcome is kept in
  `last-data-operation.json`.
- Database changes: add a numbered migration in `native/core/src/store.rs`
  (each step idempotent, run in one transaction after the automatic copy) and
  raise `SCHEMA_VERSION`. After each release, add its tag to `RELEASES` in
  `scripts/generate-upgrade-fixtures.mjs` and run
  `node scripts/generate-upgrade-fixtures.mjs v<version>`; the fixture is
  written by that release's own engine.
