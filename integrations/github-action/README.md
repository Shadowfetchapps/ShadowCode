# ShadowCode GitHub Action

Run a ShadowCode task in GitHub Actions: comment `/shadowcode fix the flaky
login test` on an issue and get a pull request back, or label a pull request
for a read-only review posted as a comment.

The action downloads a pinned ShadowCode release, checks it against the
release's `SHA256SUMS` (and, if you give one, your own SHA-256 pin), runs
`shadowcode run --json` headless with your API key, and then opens a pull
request or posts a comment with the workflow's `GITHUB_TOKEN`. It needs no
display, no FUSE and no desktop: the AppImage is extracted once and its
command line runs directly.

## Quick start

1. Add an API key as a repository secret. For [OpenRouter](https://openrouter.ai)
   call it `OPENROUTER_API_KEY`.
2. Copy an example into `.github/workflows/`:
   - [`examples/issue-comment.yml`](examples/issue-comment.yml): `/shadowcode <task>`
     on an issue opens a pull request.
   - [`examples/pr-label.yml`](examples/pr-label.yml): the `shadowcode-review`
     label on a pull request posts a read-only review.
3. Pin the action to a release tag or a full commit SHA, and set `version`
   to the ShadowCode release you want to run.

```yaml
- uses: actions/checkout@v4
  with:
    persist-credentials: false
- uses: Shadowfetchapps/ShadowCode/integrations/github-action@<tag-or-sha>
  with:
    task: ${{ github.event.comment.body }}
    api-key: ${{ secrets.OPENROUTER_API_KEY }}
    output: pull-request
```

## Inputs

| Input               | Default                           | What it does                                                                                                       |
| ------------------- | --------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `task`              | (required)                        | What to do. A leading `prefix` is removed, so a whole comment can be passed.                                       |
| `prefix`            | `/shadowcode`                     | Command prefix stripped from `task`.                                                                               |
| `api-key`           | (required)                        | The model's API key, from secrets.                                                                                 |
| `model`             | `api:openrouter:qwen/qwen3-coder` | An OpenRouter model (`api:openrouter:<slug>`), or with `endpoint`, the model name that endpoint serves.            |
| `endpoint`          | empty                             | An OpenAI-compatible base URL, for providers other than OpenRouter.                                                |
| `mode`              | `code`                            | `code` may edit files; `plan` and `ask` are read-only.                                                             |
| `approval`          | `cancel`                          | When the task asks for approval (to run a command, for example): `cancel` stops it; `approve` grants its requests. |
| `output`            | `comment`                         | `pull-request`, `comment` or `none`.                                                                               |
| `github-token`      | `github.token`                    | Used only for the comment or pull request, after ShadowCode finished.                                              |
| `issue-number`      | the event's issue or pull request | Where to comment.                                                                                                  |
| `base`              | the checked-out branch            | Target branch of the pull request.                                                                                 |
| `version`           | `0.33.1`                          | ShadowCode release to run. `approval: approve` needs the release that ships this action or later.                  |
| `appimage-sha256`   | empty                             | Optional extra pin for the AppImage's SHA-256.                                                                     |
| `working-directory` | the workspace                     | Project folder.                                                                                                    |

## Outputs

`exit-code` (0 done, 1 failed, 2 stopped for approval), `status`
(`completed`, `failed`, `needs_approval`, …), `changed` (`true` when files
changed), `result-file` (the JSON result of `shadowcode run --json`) and
`pull-request-url`.

## What it does, step by step

1. **Checks the inputs** and that the runner is Linux x64.
2. **Downloads** `ShadowCode_<version>_amd64.AppImage` and `SHA256SUMS` from
   the GitHub release over HTTPS, refuses to continue unless the checksum
   matches (and your `appimage-sha256`, if set), and extracts it.
3. **Runs the task** with a fresh profile in `$RUNNER_TEMP`: trusts the
   checkout, sets permissions to "allow edits" (shell commands still ask),
   and runs `shadowcode run --json --approval <approval>`. Only this step
   receives the API key, and ShadowCode starts the agent's shell commands
   with a minimal environment that does not include it.
4. **Delivers the result** in a separate step with the token: for
   `pull-request`, when files changed and the task succeeded, commits to a
   new `shadowcode/run-<id>` branch, pushes it, opens a pull request and
   links it on the issue; otherwise it posts the summary as a comment.

## Security

- **Trusted people only.** The issue example runs only when the comment's
  `author_association` is `OWNER`, `MEMBER` or `COLLABORATOR`. Without that
  check anyone who can comment could spend your API key and have code
  changed.
- **Never `pull_request_target` with untrusted code.** The review example
  uses `pull_request`, which gives fork pull requests no secrets and a
  read-only token, and skips forks entirely. Do not switch it to
  `pull_request_target` and check out the pull request's head: that would run
  a stranger's code with your secrets.
- **No credentials in the checkout.** Use `persist-credentials: false` so the
  agent cannot read a token from `.git/config`. The action pushes with the
  token only after the agent has finished.
- **Least privilege.** The examples set `permissions: {}` at the top and
  grant each job only `contents`, `issues` and `pull-requests` as needed.
- **Approvals.** `approval: approve` grants every request the task makes,
  such as running the test suite. Use it only on GitHub-hosted runners,
  which are discarded after the job, never on a self-hosted runner that
  keeps state. Anything the project's permissions deny stays denied.
- **Untrusted text.** Inputs reach the scripts as environment variables,
  never pasted into shell code, so a comment cannot inject commands into the
  workflow. Issue text can still try to steer the model; review every pull
  request it opens before merging.
- **Pinning.** Pin this action to a commit SHA and set `version`; add
  `appimage-sha256` to pin the exact AppImage bytes you reviewed.

## Other providers

Any OpenAI-compatible API works: set `endpoint` to its base URL, `model` to
its model name and `api-key` to its key.

```yaml
with:
  task: ${{ github.event.comment.body }}
  endpoint: https://api.example.com/v1
  model: example-coder-large
  api-key: ${{ secrets.EXAMPLE_API_KEY }}
```

See [docs/AUTOMATIONS.md](../../docs/AUTOMATIONS.md) for scheduled
automations in the desktop and "Start from an issue".
