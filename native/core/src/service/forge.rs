//! `/api/git…`: the drawer's Git panel — branches, suggested commit and pull
//! request text, push, and pull requests through `gh` (GitHub) or `glab`
//! (GitLab) with their CI checks. Staging and the commit itself stay on
//! `/api/workspace/git/{add,commit}` (`git.rs`).
//!
//! Push and the forge CLIs run with the user's own Git and SSH environment
//! (agent socket, askpass, credential helpers, `gh`/`glab` sign-in) and never
//! see or store a credential here; prompts are disabled so nothing can hang
//! waiting for a password. Repository hooks stay disabled, as for commits.
//! Remote URLs are reported without any user-info they contain.
use super::*;
use std::collections::BTreeMap;

const PUSH_TIMEOUT: Duration = Duration::from_secs(180);
pub(super) const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const MODEL_TIMEOUT: Duration = Duration::from_secs(60);
/// Diff text offered to a model when drafting a message.
const DIFF_BUDGET: usize = 16_000;

#[derive(Default, Deserialize)]
#[serde(default)]
struct GitFlowBody {
    name: Text,
    create: Flag,
    kind: Text,
    base: Text,
    remote: Text,
    title: Text,
    body: Text,
    draft: Flag,
    /// Push even though the commits look like they contain a secret.
    allow_secrets: Flag,
}

/// A remote in words the UI can use: where it lives and its web page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RemoteInfo {
    pub host: String,
    /// `owner/repo` (GitLab: `group/subgroup/repo`).
    pub path: String,
    pub web_url: String,
    /// `github`, `gitlab`, or `other`.
    pub kind: &'static str,
}

/// Parse a Git remote URL (scp-like SSH, `ssh://`, `https://`, `git://`).
pub fn parse_remote(url: &str) -> Option<RemoteInfo> {
    let url = url.trim();
    let (host, path, port) = if let Some((scheme, rest)) = url.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(
            scheme.as_str(),
            "https" | "http" | "ssh" | "git" | "git+ssh"
        ) {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let authority = authority.rsplit('@').next()?;
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
                (host, (scheme.starts_with("http")).then(|| port.to_owned()))
            }
            _ => (authority, None),
        };
        (host.to_owned(), path.to_owned(), port)
    } else {
        // user@host:owner/repo
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') || path.starts_with("//") {
            return None;
        }
        let host = authority.rsplit('@').next()?;
        (host.to_owned(), path.to_owned(), None)
    };
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_matches('/')
        .to_owned();
    if host.is_empty()
        || path.split('/').filter(|s| !s.is_empty()).count() < 2
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        || path.contains("..")
        || path.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    let lower = host.to_ascii_lowercase();
    let kind = if lower.contains("github") {
        "github"
    } else if lower.contains("gitlab") {
        "gitlab"
    } else {
        "other"
    };
    let web_host = match port {
        Some(port) => format!("{host}:{port}"),
        None => host.clone(),
    };
    Some(RemoteInfo {
        web_url: format!("https://{web_host}/{path}"),
        host,
        path,
        kind,
    })
}

/// Percent-encode a branch name for a URL path or query value.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The forge's "open a pull/merge request" page for `head` into `base`.
pub fn compare_url(remote: &RemoteInfo, base: &str, head: &str) -> Option<String> {
    match remote.kind {
        "github" => Some(format!(
            "{}/compare/{}...{}?expand=1",
            remote.web_url,
            encode(base),
            encode(head)
        )),
        "gitlab" => Some(format!(
            "{}/-/merge_requests/new?merge_request%5Bsource_branch%5D={}&merge_request%5Btarget_branch%5D={}",
            remote.web_url,
            encode(head),
            encode(base)
        )),
        _ => None,
    }
}

/// A branch name the user typed: Git's own rules plus a few that keep it
/// safe to pass as an argument and readable in a URL.
pub fn validate_branch(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "Enter a branch name");
    ensure!(name.len() <= 200, "Branch names are at most 200 characters");
    ensure!(
        !name.starts_with('-'),
        "A branch name cannot start with a dash"
    );
    ensure!(
        !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c)),
        "A branch name cannot contain spaces or any of ~ ^ : ? * [ \\"
    );
    ensure!(
        !name.contains("..")
            && !name.contains("@{")
            && !name.contains("//")
            && name != "@"
            && name != "HEAD"
            && !name.starts_with('/')
            && !name.ends_with('/')
            && !name.ends_with('.')
            && !name.ends_with(".lock")
            && !name.split('/').any(|part| part.starts_with('.')),
        "That is not a valid branch name"
    );
    Ok(())
}

/// The user's environment for Git transport and forge CLIs. `process::run`
/// starts from a minimal environment; these add what push and `gh`/`glab`
/// need to use the user's own sign-in. Values pass through; none are read.
pub(super) fn user_tool_env() -> BTreeMap<String, String> {
    const PASS: &[&str] = &[
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "SSH_ASKPASS",
        "GIT_ASKPASS",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_CONFIG_GLOBAL",
        "GNUPGHOME",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_HOST",
        "GH_CONFIG_DIR",
        "GITLAB_TOKEN",
        "GITLAB_HOST",
        "GLAB_CONFIG_DIR",
    ];
    let mut env: BTreeMap<String, String> = PASS
        .iter()
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect();
    for (name, value) in [
        ("GH_PROMPT_DISABLED", "1"),
        ("GH_NO_UPDATE_NOTIFIER", "1"),
        ("GH_SPINNER_DISABLED", "1"),
        ("GLAB_NO_PROMPT", "1"),
        ("NO_PROMPT", "1"),
        ("GLAB_CHECK_UPDATE", "false"),
        ("CLICOLOR", "0"),
    ] {
        env.insert(name.into(), value.into());
    }
    env
}

pub(super) async fn run_tool(
    workspace: &Path,
    program: &str,
    args: Vec<String>,
    timeout: Duration,
) -> Result<process::ProcessResult> {
    let mut spec = ProcessSpec::command(program, &[], workspace.to_owned());
    spec.args = args;
    spec.timeout = timeout;
    spec.env = user_tool_env();
    process::run(spec, CancellationToken::new(), None).await
}

/// Git with the user's transport environment (push). Same safety flags as
/// every other Git call here.
async fn git_user(
    workspace: &Path,
    args: Vec<String>,
    timeout: Duration,
) -> Result<process::ProcessResult> {
    let mut all: Vec<String> = [
        "--no-pager",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "color.ui=false",
        "-c",
        "core.quotepath=false",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    all.extend(args);
    run_tool(workspace, "git", all, timeout).await
}

fn out(value: &Value) -> String {
    value["stdout"].as_str().unwrap_or("").trim().to_owned()
}
fn ok(value: &Value) -> bool {
    value["ok"] == true
}
/// Tool output for an error message, with anything secret-shaped removed.
pub(super) fn clean(text: &str) -> String {
    let text = crate::redaction::redact_text(text.trim()).text;
    truncate(&text, 2000).to_owned()
}
fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A drafted message: the text and where it came from.
pub(super) struct Draft {
    pub(super) text: String,
    pub(super) source: &'static str,
    pub(super) model: String,
    note: String,
}

impl Service {
    pub(super) async fn forge_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let body: GitFlowBody = call.body()?;
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/git") => self.git_overview(call.q("remote")).await,
            ("POST", "/api/git/branch") => {
                self.git_branch(body.name.as_str(), body.create.is_true())
                    .await
            }
            ("POST", "/api/git/suggest") => {
                self.git_suggest(body.kind.as_str(), body.base.as_str(), body.remote.as_str())
                    .await
            }
            ("POST", "/api/git/push") => {
                self.git_push(body.remote.as_str(), body.allow_secrets.is_true())
                    .await
            }
            ("GET", "/api/git/pr") => self.pr_status(call.q("remote"), call.q("base")).await,
            ("POST", "/api/git/pr") => self.pr_create(&body).await,
            ("GET", "/api/git/pr/checks") => {
                self.pr_checks(call.q("remote"), call.q("number")).await
            }
            _ => Err(call.unavailable()),
        }
    }

    async fn git_read(&self, workspace: &Path, list: &[&str]) -> Result<Value> {
        Self::git_in(workspace, args(list), CancellationToken::new()).await
    }

    /// Remotes with their parsed forge, never with credentials.
    async fn remotes(&self, workspace: &Path) -> Result<Vec<(String, Option<RemoteInfo>)>> {
        let listed = self.git_read(workspace, &["remote"]).await?;
        let mut remotes = Vec::new();
        for name in out(&listed)
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let url = self
                .git_read(workspace, &["remote", "get-url", "--", name])
                .await?;
            remotes.push((name.to_owned(), parse_remote(&out(&url))));
        }
        Ok(remotes)
    }

    async fn current_branch(&self, workspace: &Path) -> Result<Option<String>> {
        let head = self
            .git_read(workspace, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await?;
        Ok(ok(&head).then(|| out(&head)).filter(|b| !b.is_empty()))
    }

    /// The upstream of the current branch as `(remote, branch)`.
    async fn upstream(&self, workspace: &Path) -> Result<Option<(String, String)>> {
        let Some(branch) = self.current_branch(workspace).await? else {
            return Ok(None);
        };
        let upstream = self
            .git_read(
                workspace,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            )
            .await?;
        if !ok(&upstream) {
            return Ok(None);
        }
        let full = out(&upstream);
        let remote = self
            .git_read(
                workspace,
                &["config", "--get", &format!("branch.{branch}.remote")],
            )
            .await?;
        let remote = out(&remote);
        Ok(match full.strip_prefix(&format!("{remote}/")) {
            Some(tracked) if !remote.is_empty() => Some((remote.clone(), tracked.to_owned())),
            _ => full
                .split_once('/')
                .map(|(r, b)| (r.to_owned(), b.to_owned())),
        })
    }

    /// Which remote the panel works with: the requested one, the branch's
    /// upstream remote, `origin`, or the first.
    async fn chosen_remote(
        &self,
        workspace: &Path,
        requested: &str,
    ) -> Result<Option<(String, Option<RemoteInfo>)>> {
        let remotes = self.remotes(workspace).await?;
        if !requested.is_empty() {
            return Ok(Some(
                remotes
                    .into_iter()
                    .find(|(name, _)| name == requested)
                    .context("That remote is not configured in this repository")?,
            ));
        }
        let upstream = self.upstream(workspace).await?.map(|(r, _)| r);
        let pick = upstream
            .and_then(|u| remotes.iter().find(|(n, _)| *n == u).cloned())
            .or_else(|| remotes.iter().find(|(n, _)| n == "origin").cloned())
            .or_else(|| remotes.first().cloned());
        Ok(pick)
    }

    /// The branch a pull request most likely targets.
    async fn default_base(&self, workspace: &Path, remote: Option<&str>) -> Result<String> {
        if let Some(remote) = remote {
            let head = self
                .git_read(
                    workspace,
                    &[
                        "symbolic-ref",
                        "--quiet",
                        "--short",
                        &format!("refs/remotes/{remote}/HEAD"),
                    ],
                )
                .await?;
            if let Some(branch) = out(&head).strip_prefix(&format!("{remote}/")) {
                if ok(&head) && !branch.is_empty() {
                    return Ok(branch.to_owned());
                }
            }
        }
        for candidate in ["main", "master", "trunk", "develop"] {
            let refs = match remote {
                Some(remote) => vec![
                    format!("refs/remotes/{remote}/{candidate}"),
                    format!("refs/heads/{candidate}"),
                ],
                None => vec![format!("refs/heads/{candidate}")],
            };
            for reference in refs {
                let found = self
                    .git_read(workspace, &["show-ref", "--verify", "--quiet", &reference])
                    .await?;
                if ok(&found) {
                    return Ok(candidate.to_owned());
                }
            }
        }
        Ok("main".into())
    }

    /// GET /api/git: branch, upstream, ahead/behind, branches, remotes and
    /// pull request bases for the selected project.
    async fn git_overview(&self, requested_remote: &str) -> Result<Value> {
        let workspace = self.workspace()?;
        let inside = self
            .git_read(&workspace, &["rev-parse", "--is-inside-work-tree"])
            .await?;
        if !ok(&inside) {
            return Ok(json!({"repo": false}));
        }
        let branch = self.current_branch(&workspace).await?;
        let has_commits = ok(&self
            .git_read(&workspace, &["rev-parse", "--verify", "--quiet", "HEAD"])
            .await?);
        let upstream = self.upstream(&workspace).await?;
        let (mut ahead, mut behind) = (0u64, 0u64);
        if upstream.is_some() {
            let counts = self
                .git_read(
                    &workspace,
                    &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
                )
                .await?;
            let text = out(&counts);
            let mut parts = text.split_whitespace();
            behind = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            ahead = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        } else if has_commits {
            // Not pushed yet: commits no remote-tracking branch has.
            let unpublished = self
                .git_read(
                    &workspace,
                    &["rev-list", "--count", "HEAD", "--not", "--remotes"],
                )
                .await?;
            ahead = out(&unpublished).parse().unwrap_or(0);
        }
        let local = self
            .git_read(
                &workspace,
                &[
                    "for-each-ref",
                    "--sort=-committerdate",
                    "--count=200",
                    "--format=%(refname:short)%09%(upstream:short)",
                    "refs/heads",
                ],
            )
            .await?;
        let branches: Vec<Value> = out(&local)
            .lines()
            .filter_map(|line| {
                let (name, upstream) = line.split_once('\t').unwrap_or((line, ""));
                (!name.is_empty()).then(|| {
                    json!({"name": name, "upstream": upstream, "current": Some(name) == branch.as_deref()})
                })
            })
            .collect();
        let remotes = self.remotes(&workspace).await?;
        let chosen = self.chosen_remote(&workspace, requested_remote).await?;
        let remote_name = chosen.as_ref().map(|(n, _)| n.clone());
        let mut bases: Vec<String> = Vec::new();
        if let Some(remote) = &remote_name {
            let listed = self
                .git_read(
                    &workspace,
                    &[
                        "for-each-ref",
                        "--count=300",
                        "--format=%(refname:short)",
                        &format!("refs/remotes/{remote}"),
                    ],
                )
                .await?;
            bases = out(&listed)
                .lines()
                .filter_map(|r| r.strip_prefix(&format!("{remote}/")))
                .filter(|b| *b != "HEAD" && !b.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if bases.is_empty() {
            bases = branches
                .iter()
                .filter_map(|b| b["name"].as_str().map(str::to_owned))
                .collect();
        }
        let default_base = self
            .default_base(&workspace, remote_name.as_deref())
            .await?;
        if !bases.contains(&default_base) {
            bases.insert(0, default_base.clone());
        }
        let staged = self
            .git_read(&workspace, &["diff", "--cached", "--name-only", "-z"])
            .await?;
        let staged = out(&staged).split('\0').filter(|s| !s.is_empty()).count();
        let changed = self
            .git_read(&workspace, &["status", "--porcelain=v1", "-z"])
            .await?;
        let changed = out(&changed).split('\0').filter(|s| s.len() > 3).count();
        Ok(json!({
            "repo": true,
            "branch": branch,
            "detached": branch.is_none() && has_commits,
            "has_commits": has_commits,
            "upstream": upstream.as_ref().map(|(r, b)| format!("{r}/{b}")),
            "ahead": ahead,
            "behind": behind,
            "staged": staged,
            "changed": changed,
            "branches": branches,
            "remotes": remotes.iter().map(|(name, info)| json!({"name": name, "info": info})).collect::<Vec<_>>(),
            "remote": remote_name,
            "remote_info": chosen.and_then(|(_, info)| info),
            "bases": bases,
            "default_base": default_base,
        }))
    }

    /// POST /api/git/branch: switch to a branch, or create it from HEAD.
    async fn git_branch(&self, name: &str, create: bool) -> Result<Value> {
        let name = name.trim();
        validate_branch(name)?;
        let ws = self.mutable_workspace()?;
        let check = Self::git_in(
            &ws.path,
            args(&["check-ref-format", "--branch", name]),
            ws.reservation.cancellation(),
        )
        .await?;
        ensure!(ok(&check), "That is not a valid branch name");
        let mut command = args(&["switch"]);
        if create {
            command.push("-c".into());
        }
        command.push(name.into());
        let result = Self::git_in(&ws.path, command, ws.reservation.cancellation()).await?;
        ensure!(
            ok(&result),
            "{}",
            clean(
                result["stderr"]
                    .as_str()
                    .unwrap_or("Could not switch branch")
            )
        );
        Ok(json!({"ok": true, "branch": name, "created": create}))
    }

    /// The in-band refusal when the commits a push would send look like
    /// they contain a secret (`secret_scan`), unless the user allowed it.
    async fn push_secret_check(
        &self,
        workspace: &Path,
        requested_remote: &str,
        allow: bool,
    ) -> Result<Option<Value>> {
        if allow {
            return Ok(None);
        }
        let Some((remote, _)) = self.chosen_remote(workspace, requested_remote).await? else {
            return Ok(None);
        };
        let scan = crate::secret_scan::unpushed(workspace, &remote).await?;
        Ok((!scan.is_clean()).then(|| scan.refusal("pushed")))
    }

    /// POST /api/git/push `{remote?, allow_secrets?}`: push the current
    /// branch and set its upstream. Commits that look like they contain a
    /// secret are refused in band first.
    async fn git_push(&self, requested_remote: &str, allow_secrets: bool) -> Result<Value> {
        let workspace = self.workspace()?;
        if let Some(refusal) = self
            .push_secret_check(&workspace, requested_remote, allow_secrets)
            .await?
        {
            return Ok(refusal);
        }
        let (remote, branch, output) = self.push_current(&workspace, requested_remote).await?;
        let info = self
            .remotes(&workspace)
            .await?
            .into_iter()
            .find(|(name, _)| *name == remote)
            .and_then(|(_, info)| info);
        Ok(json!({
            "ok": true,
            "remote": remote,
            "branch": branch,
            "output": output,
            "remote_info": info,
        }))
    }

    async fn push_current(
        &self,
        workspace: &Path,
        requested_remote: &str,
    ) -> Result<(String, String, String)> {
        let branch = self
            .current_branch(workspace)
            .await?
            .context("Switch to a branch before pushing (the project is on a detached commit)")?;
        let remote = match self.chosen_remote(workspace, requested_remote).await? {
            Some((name, _)) => name,
            None => bail!("This repository has no remote to push to. Add one with git remote add."),
        };
        // Always to the same-named branch; never forced.
        let result = git_user(
            workspace,
            vec![
                "push".into(),
                "--set-upstream".into(),
                "--porcelain".into(),
                "--".into(),
                remote.clone(),
                format!("refs/heads/{branch}:refs/heads/{branch}"),
            ],
            PUSH_TIMEOUT,
        )
        .await?;
        if !result.ok {
            let detail = clean(&format!("{}\n{}", result.stderr, result.stdout));
            let hint = if result.timed_out {
                " The push timed out."
            } else if detail.contains("terminal prompts disabled")
                || detail.contains("could not read Username")
                || detail.contains("Permission denied")
                || detail.contains("Authentication failed")
            {
                " Git could not sign in. Set up an SSH key or a credential helper (for GitHub, `gh auth setup-git`), then try again."
            } else if detail.contains("rejected") || detail.contains("non-fast-forward") {
                " The remote has commits you do not have. Pull them first, then push again."
            } else {
                ""
            };
            bail!("Push failed.{hint}\n{detail}");
        }
        Ok((
            remote,
            branch,
            clean(&format!("{}{}", result.stdout, result.stderr)),
        ))
    }

    // --- Suggested text ------------------------------------------------------

    /// POST /api/git/suggest: a commit message from the staged diff
    /// (`kind: commit`) or a pull request title and body from the branch's
    /// commits (`kind: pr`). Tries the conversation's model when it runs
    /// through ShadowCode (API, OpenRouter or local), then a loaded local
    /// model, then writes a plain summary. Always editable in the UI.
    async fn git_suggest(&self, kind: &str, base: &str, remote: &str) -> Result<Value> {
        let workspace = self.workspace()?;
        let pr = match kind {
            "" | "commit" => false,
            "pr" => true,
            _ => bail!("Choose commit or pr"),
        };
        let (facts, fallback, system) = if pr {
            self.pr_facts(&workspace, base, remote).await?
        } else {
            self.commit_facts(&workspace).await?
        };
        let draft = self.draft_with_model(&workspace, &system, &facts).await;
        let draft = match draft {
            Ok(Some(draft)) if !draft.text.trim().is_empty() => draft,
            Ok(_) => Draft {
                text: fallback,
                source: "summary",
                model: String::new(),
                note:
                    "No ShadowCode model is available, so this is a plain summary of the changes."
                        .into(),
            },
            Err(error) => Draft {
                text: fallback,
                source: "summary",
                model: String::new(),
                note: format!(
                    "The model could not draft this ({}); showing a plain summary.",
                    truncate(&format!("{error:#}"), 200)
                ),
            },
        };
        let mut result = json!({
            "kind": if pr { "pr" } else { "commit" },
            "source": draft.source,
            "model": draft.model,
            "note": draft.note,
        });
        if pr {
            let (title, body) = split_title(&draft.text);
            result["title"] = json!(title);
            result["body"] = json!(body);
        } else {
            result["message"] = json!(tidy_message(&draft.text));
        }
        Ok(result)
    }

    /// Staged changes as model input, a deterministic message, and the
    /// instructions for the model.
    async fn commit_facts(&self, workspace: &Path) -> Result<(String, String, String)> {
        let names = self
            .git_read(
                workspace,
                &["diff", "--cached", "--name-status", "--no-renames", "-z"],
            )
            .await?;
        ensure!(ok(&names), "Could not read the staged changes");
        let raw = names["stdout"].as_str().unwrap_or("");
        let mut fields = raw.split('\0').filter(|s| !s.is_empty());
        let mut files: Vec<(String, String)> = Vec::new();
        while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
            files.push((status.to_owned(), path.to_owned()));
        }
        ensure!(
            !files.is_empty(),
            "Nothing is staged yet. Stage the changes to commit first."
        );
        let numstat = self
            .git_read(
                workspace,
                &["diff", "--cached", "--numstat", "--no-renames", "-z"],
            )
            .await?;
        let counts = parse_numstat(numstat["stdout"].as_str().unwrap_or(""));
        let fallback = summary_message(&files, &counts);
        let shown: Vec<String> = files
            .iter()
            .map(|(_, p)| p.clone())
            .filter(|p| !crate::redaction::is_secret_path(p))
            .take(200)
            .collect();
        let mut diff = String::new();
        if !shown.is_empty() {
            let mut command = args(&[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--unified=2",
                "--",
            ]);
            command.extend(shown);
            let text = Self::git_in(workspace, command, CancellationToken::new()).await?;
            diff = out(&text);
        }
        let listing = files
            .iter()
            .map(|(status, path)| {
                let (add, del) = counts.get(path).copied().unwrap_or((0, 0));
                format!("{status}\t{path}\t+{add} -{del}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let facts = format!(
            "Staged files (status, path, lines added/removed):\n{listing}\n\nStaged diff (may be cut short):\n{}",
            truncate(&diff, DIFF_BUDGET)
        );
        let system = "You write Git commit messages. Reply with the message only: a subject line of at most 72 characters in the imperative mood (\"Add\", \"Fix\", \"Update\"), a blank line, then a short body of plain sentences or '-' bullets saying what changed and why. No code fences, quotes, headings or preamble.".to_owned();
        Ok((facts, fallback, system))
    }

    async fn pr_facts(
        &self,
        workspace: &Path,
        base: &str,
        remote: &str,
    ) -> Result<(String, String, String)> {
        let branch = self.current_branch(workspace).await?.unwrap_or_default();
        let remote = self.chosen_remote(workspace, remote).await?.map(|(n, _)| n);
        let base = match base.trim() {
            "" => self.default_base(workspace, remote.as_deref()).await?,
            base => {
                validate_branch(base)?;
                base.to_owned()
            }
        };
        let mut base_ref = base.clone();
        if let Some(remote) = &remote {
            let candidate = format!("refs/remotes/{remote}/{base}");
            if ok(&self
                .git_read(workspace, &["show-ref", "--verify", "--quiet", &candidate])
                .await?)
            {
                base_ref = format!("{remote}/{base}");
            }
        }
        let range = format!("{base_ref}..HEAD");
        let log = self
            .git_read(
                workspace,
                &[
                    "log",
                    "--no-merges",
                    "--max-count=50",
                    "--format=%s%x1f%b%x1e",
                    &range,
                    "--",
                ],
            )
            .await?;
        let commits: Vec<(String, String)> = out(&log)
            .split('\u{1e}')
            .filter_map(|record| {
                let (subject, body) = record.trim().split_once('\u{1f}')?;
                (!subject.trim().is_empty())
                    .then(|| (subject.trim().to_owned(), body.trim().to_owned()))
            })
            .collect();
        let stat = self
            .git_read(
                workspace,
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--stat=100",
                    &format!("{base_ref}...HEAD"),
                    "--",
                ],
            )
            .await?;
        let stat = out(&stat);
        let fallback = summary_pr(&branch, &commits, &stat);
        let range3 = format!("{base_ref}...HEAD");
        let names = self
            .git_read(workspace, &["diff", "--name-only", "-z", &range3, "--"])
            .await?;
        let shown: Vec<String> = out(&names)
            .split('\0')
            .filter(|p| !p.is_empty() && !crate::redaction::is_secret_path(p))
            .take(200)
            .map(str::to_owned)
            .collect();
        let mut diff = String::new();
        if !shown.is_empty() {
            let mut command = args(&[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--unified=2",
                &range3,
                "--",
            ]);
            command.extend(shown);
            diff = out(&Self::git_in(workspace, command, CancellationToken::new()).await?);
        }
        let log_text = commits
            .iter()
            .map(|(s, b)| {
                if b.is_empty() {
                    format!("- {s}")
                } else {
                    format!("- {s}\n  {}", b.replace('\n', "\n  "))
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let facts = format!(
            "Branch {branch} into {base}.\n\nCommits:\n{log_text}\n\nFiles changed:\n{stat}\n\nDiff (may be cut short):\n{}",
            truncate(&diff, DIFF_BUDGET)
        );
        let system = "You write pull request descriptions. Reply exactly in this form and nothing else:\nTitle: <one line, at most 72 characters>\n\n<a short summary paragraph>\n\n## Changes\n- <bullet per notable change>\n\nNo code fences around the reply and no preamble.".to_owned();
        Ok((facts, fallback, system))
    }

    /// Ask a model that runs through ShadowCode. `Ok(None)` when none is
    /// available (a subscription conversation with no local model loaded).
    pub(super) async fn draft_with_model(
        &self,
        workspace: &Path,
        system: &str,
        facts: &str,
    ) -> Result<Option<Draft>> {
        let cfg = self.config()?;
        let store = self.engine.store();
        let target = match self.current_session()? {
            Some(sid) => store.session_meta(&sid, crate::store::keys::EXECUTION_TARGET)?,
            None => None,
        }
        .or(store.native_meta(&crate::store::keys::execution_target(workspace))?);
        let conversation = match &target {
            Some(id) => self.resolve_model(id, &cfg.model).ok(),
            None => Some(cfg.model.clone()),
        };
        let usable = |model: &ModelConfig| {
            model.provider != "mock"
                && crate::cli_agent::Vendor::from_provider(&model.provider).is_none()
                && (!cfg.offline()
                    || crate::local_engine::is_managed(model)
                    || models::is_loopback_endpoint(&model.endpoint))
        };
        let (model, source) = match conversation.filter(usable) {
            Some(model) => (model, "model"),
            None => match self.engine.local_runtime().loaded() {
                Some(loaded) => match self.resolve_model(&loaded.id, &cfg.model) {
                    Ok(model) => (model, "local"),
                    Err(_) => return Ok(None),
                },
                None => return Ok(None),
            },
        };
        // A paid model's draft counts toward today's spending, and none is
        // sent once today's limit is reached.
        crate::spending::check_outside_task(&store, &cfg.spending, &model, crate::now())?;
        let facts = crate::redaction::redact_text(facts).text;
        let cancel = CancellationToken::new();
        let attempt = async {
            let prepared = self
                .engine
                .prepare_model_client(&cfg, &model, &cancel)
                .await?;
            let client = prepared.client(self.engine.paths())?;
            let name = prepared.config.name.clone();
            let reply = client
                .chat(
                    &[
                        json!({"role": "system", "content": system}),
                        json!({"role": "user", "content": facts}),
                    ],
                    &[],
                    cancel.clone(),
                    |_| {},
                )
                .await?;
            let mut usage = reply.usage;
            crate::usage::price_turn(&mut usage, &prepared.config, &self.engine.paths().state);
            crate::spending::record_outside_task(&store, &prepared.config, &usage)?;
            Ok::<_, anyhow::Error>((reply.text, name))
        };
        match tokio::time::timeout(MODEL_TIMEOUT, attempt).await {
            Ok(Ok((text, name))) => {
                let text = strip_reasoning(&text);
                Ok((!text.trim().is_empty()).then(|| Draft {
                    text,
                    source,
                    note: String::new(),
                    model: if name.is_empty() {
                        model.name.clone()
                    } else {
                        name
                    },
                }))
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                cancel.cancel();
                bail!("the model took longer than a minute")
            }
        }
    }

    // --- Pull requests -------------------------------------------------------

    /// Which forge CLI applies to a remote and whether it is ready.
    pub(super) async fn forge_cli(&self, workspace: &Path, info: &RemoteInfo) -> Value {
        let (program, install, login) = match info.kind {
            "github" => (
                "gh",
                "https://cli.github.com",
                format!("gh auth login --hostname {}", info.host),
            ),
            "gitlab" => (
                "glab",
                "https://gitlab.com/gitlab-org/cli#installation",
                format!("glab auth login --hostname {}", info.host),
            ),
            _ => return json!({"name": null, "installed": false, "authenticated": false}),
        };
        let version = run_tool(
            workspace,
            program,
            args(&["--version"]),
            Duration::from_secs(15),
        )
        .await;
        let Ok(version) = version else {
            return json!({"name": program, "installed": false, "authenticated": false, "install_url": install, "login_command": login});
        };
        let version_line = version
            .stdout
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_owned();
        let status = run_tool(
            workspace,
            program,
            args(&["auth", "status", "--hostname", &info.host]),
            Duration::from_secs(20),
        )
        .await;
        let (authenticated, detail) = match status {
            Ok(status) => {
                let text = format!("{}\n{}", status.stdout, status.stderr);
                let line = text
                    .lines()
                    .map(str::trim)
                    .find(|l| l.contains("Logged in") || l.contains("logged in"))
                    .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
                    .unwrap_or("")
                    .trim_start_matches(['✓', 'X', '!', '-', ' '])
                    .to_owned();
                (status.ok, clean(&line))
            }
            Err(error) => (false, clean(&format!("{error:#}"))),
        };
        json!({
            "name": program,
            "installed": true,
            "version": version_line,
            "authenticated": authenticated,
            "detail": detail,
            "install_url": install,
            "login_command": login,
        })
    }

    /// `owner/repo` for github.com, `HOST/owner/repo` elsewhere.
    pub(super) fn repo_arg(info: &RemoteInfo) -> String {
        if info.host.eq_ignore_ascii_case("github.com")
            || info.host.eq_ignore_ascii_case("gitlab.com")
        {
            info.path.clone()
        } else {
            format!("{}/{}", info.host, info.path)
        }
    }

    pub(super) async fn forge_remote(
        &self,
        workspace: &Path,
        remote: &str,
    ) -> Result<(String, RemoteInfo)> {
        match self.chosen_remote(workspace, remote).await? {
            Some((name, Some(info))) => Ok((name, info)),
            Some((name, None)) => bail!("Remote {name} is not a web address ShadowCode recognises"),
            None => bail!("This repository has no remote. Add one with git remote add."),
        }
    }

    /// GET /api/git/pr: the forge, its CLI's readiness, the compare page and
    /// an existing pull request for the current branch.
    async fn pr_status(&self, remote: &str, base: &str) -> Result<Value> {
        let workspace = self.workspace()?;
        let branch = self.current_branch(&workspace).await?;
        let (name, info) = match self.chosen_remote(&workspace, remote).await? {
            Some((name, Some(info))) => (name, info),
            Some((name, None)) => {
                return Ok(
                    json!({"remote": name, "provider": null, "cli": {"name": null}, "pr": null}),
                )
            }
            None => {
                return Ok(
                    json!({"remote": null, "provider": null, "cli": {"name": null}, "pr": null}),
                )
            }
        };
        let base = match base.trim() {
            "" => self.default_base(&workspace, Some(&name)).await?,
            base => base.to_owned(),
        };
        let cli = self.forge_cli(&workspace, &info).await;
        let mut pr = Value::Null;
        if cli["authenticated"] == true {
            if let Some(branch) = &branch {
                pr = self
                    .find_pr(&workspace, &info, branch)
                    .await
                    .unwrap_or(Value::Null);
            }
        }
        Ok(json!({
            "remote": name,
            "provider": info.kind,
            "remote_info": info,
            "cli": cli,
            "base": base,
            "compare_url": branch.as_deref().and_then(|b| compare_url(&info, &base, b)),
            "pr": pr,
        }))
    }

    async fn find_pr(&self, workspace: &Path, info: &RemoteInfo, branch: &str) -> Result<Value> {
        match info.kind {
            "github" => {
                let result = run_tool(
                    workspace,
                    "gh",
                    vec![
                        "pr".into(),
                        "view".into(),
                        branch.into(),
                        "--repo".into(),
                        Self::repo_arg(info),
                        "--json".into(),
                        "number,url,state,isDraft,title,baseRefName,headRefName".into(),
                    ],
                    TOOL_TIMEOUT,
                )
                .await?;
                ensure!(result.ok, "No pull request");
                let pr: Value = serde_json::from_str(result.stdout.trim())?;
                Ok(json!({
                    "number": pr["number"],
                    "url": pr["url"],
                    "state": pr["state"],
                    "draft": pr["isDraft"],
                    "title": pr["title"],
                    "base": pr["baseRefName"],
                }))
            }
            "gitlab" => {
                let result = run_tool(
                    workspace,
                    "glab",
                    vec![
                        "mr".into(),
                        "view".into(),
                        branch.into(),
                        "--repo".into(),
                        Self::repo_arg(info),
                        "--output".into(),
                        "json".into(),
                    ],
                    TOOL_TIMEOUT,
                )
                .await?;
                ensure!(result.ok, "No merge request");
                let mr: Value = serde_json::from_str(result.stdout.trim())?;
                Ok(json!({
                    "number": mr["iid"],
                    "url": mr["web_url"],
                    "state": mr["state"],
                    "draft": mr["draft"],
                    "title": mr["title"],
                    "base": mr["target_branch"],
                }))
            }
            _ => bail!("Unsupported forge"),
        }
    }

    /// POST /api/git/pr: push the branch when needed, then open the pull
    /// (merge) request with the forge's CLI.
    async fn pr_create(&self, body: &GitFlowBody) -> Result<Value> {
        let workspace = self.workspace()?;
        let title = body.title.as_str().trim();
        ensure!(!title.is_empty(), "Enter a title for the pull request");
        ensure!(title.len() <= 256, "Keep the title under 256 characters");
        ensure!(
            body.body.as_str().len() <= 60_000,
            "The description is too long"
        );
        let base = body.base.as_str().trim();
        validate_branch(base).context("Choose the branch to merge into")?;
        let branch = self
            .current_branch(&workspace)
            .await?
            .context("Switch to a branch before opening a pull request")?;
        ensure!(
            branch != base,
            "Create a branch for this work first: you are on {base}"
        );
        let (remote, info) = self.forge_remote(&workspace, body.remote.as_str()).await?;
        if let Some(refusal) = self
            .push_secret_check(&workspace, &remote, body.allow_secrets.is_true())
            .await?
        {
            return Ok(refusal);
        }
        let cli = self.forge_cli(&workspace, &info).await;
        ensure!(
            cli["installed"] == true && cli["authenticated"] == true,
            "{} is not ready. Sign in with `{}` or open the compare page instead.",
            cli["name"].as_str().unwrap_or("The forge CLI"),
            cli["login_command"].as_str().unwrap_or("")
        );
        // Push when the branch has no upstream or has commits the remote lacks.
        let needs_push = match self.upstream(&workspace).await? {
            None => true,
            Some(_) => {
                let ahead = self
                    .git_read(&workspace, &["rev-list", "--count", "@{upstream}..HEAD"])
                    .await?;
                out(&ahead) != "0"
            }
        };
        if needs_push {
            self.push_current(&workspace, &remote).await?;
        }
        let repo = Self::repo_arg(&info);
        let (program, command) = match info.kind {
            "github" => {
                let mut command = vec![
                    "pr".into(),
                    "create".into(),
                    "--repo".into(),
                    repo,
                    "--base".into(),
                    base.into(),
                    "--head".into(),
                    branch.clone(),
                    "--title".into(),
                    title.into(),
                    "--body".into(),
                    body.body.as_str().into(),
                ];
                if body.draft.is_true() {
                    command.push("--draft".into());
                }
                ("gh", command)
            }
            "gitlab" => {
                let mut command = vec![
                    "mr".into(),
                    "create".into(),
                    "--repo".into(),
                    repo,
                    "--target-branch".into(),
                    base.into(),
                    "--source-branch".into(),
                    branch.clone(),
                    "--title".into(),
                    title.into(),
                    "--description".into(),
                    body.body.as_str().into(),
                    "--yes".into(),
                ];
                if body.draft.is_true() {
                    command.push("--draft".into());
                }
                ("glab", command)
            }
            _ => bail!("Pull requests can be opened here for GitHub and GitLab remotes"),
        };
        let result = run_tool(&workspace, program, command, TOOL_TIMEOUT).await?;
        let text = format!("{}\n{}", result.stdout, result.stderr);
        let url = find_request_url(&text);
        ensure!(
            result.ok && url.is_some(),
            "Could not open the pull request: {}",
            clean(&text)
        );
        let url = url.unwrap_or_default();
        let number = url.rsplit('/').next().and_then(|n| n.parse::<u64>().ok());
        Ok(json!({
            "ok": true,
            "url": url,
            "number": number,
            "provider": info.kind,
            "pushed": needs_push,
            "branch": branch,
            "base": base,
            "draft": body.draft.is_true(),
        }))
    }

    /// GET /api/git/pr/checks: CI checks of a pull request (GitHub).
    async fn pr_checks(&self, remote: &str, number: &str) -> Result<Value> {
        let workspace = self.workspace()?;
        ensure!(
            !number.is_empty() && number.len() <= 12 && number.bytes().all(|b| b.is_ascii_digit()),
            "Choose a pull request"
        );
        let (_, info) = self.forge_remote(&workspace, remote).await?;
        if info.kind != "github" {
            return Ok(json!({
                "supported": false,
                "checks": [],
                "summary": {},
                "url": format!("{}/-/merge_requests/{number}/pipelines", info.web_url),
            }));
        }
        let result = run_tool(
            &workspace,
            "gh",
            vec![
                "pr".into(),
                "checks".into(),
                number.into(),
                "--repo".into(),
                Self::repo_arg(&info),
                "--json".into(),
                "name,state,bucket,link,workflow,description".into(),
            ],
            TOOL_TIMEOUT,
        )
        .await?;
        // gh exits 8 while checks are pending and 1 when one failed; the
        // JSON is on stdout either way.
        let checks: Vec<Value> = match serde_json::from_str::<Value>(result.stdout.trim()) {
            Ok(Value::Array(rows)) => rows,
            _ if result.stderr.contains("no checks") || result.stdout.contains("no checks") => {
                Vec::new()
            }
            _ => bail!(
                "Could not read the checks: {}",
                clean(&format!("{}\n{}", result.stderr, result.stdout))
            ),
        };
        let mut summary = BTreeMap::<&str, u64>::new();
        let rows: Vec<Value> = checks
            .iter()
            .map(|check| {
                let bucket = match check["bucket"].as_str().unwrap_or("") {
                    "pass" => "pass",
                    "fail" | "cancel" => "fail",
                    "skipping" => "skipping",
                    _ => "pending",
                };
                *summary.entry(bucket).or_default() += 1;
                json!({
                    "name": check["name"],
                    "workflow": check["workflow"],
                    "state": check["state"],
                    "bucket": bucket,
                    "link": check["link"].as_str().filter(|l| l.starts_with("https://")),
                    "description": check["description"],
                })
            })
            .collect();
        let overall = if rows.is_empty() {
            "none"
        } else if summary.get("fail").copied().unwrap_or(0) > 0 {
            "fail"
        } else if summary.get("pending").copied().unwrap_or(0) > 0 {
            "pending"
        } else {
            "pass"
        };
        Ok(json!({
            "supported": true,
            "checks": rows,
            "summary": summary,
            "overall": overall,
            "url": format!("{}/pull/{number}/checks", info.web_url),
            "checked_at": crate::now(),
        }))
    }
}

/// `path -> (added, deleted)` from `git diff --numstat -z`.
fn parse_numstat(raw: &str) -> HashMap<String, (u64, u64)> {
    raw.split('\0')
        .filter_map(|record| {
            let mut parts = record.splitn(3, '\t');
            let add = parts.next()?.parse().unwrap_or(0);
            let del = parts.next()?.parse().unwrap_or(0);
            let path = parts.next()?.trim();
            (!path.is_empty()).then(|| (path.to_owned(), (add, del)))
        })
        .collect()
}

fn verb(status: &str) -> &'static str {
    match status.chars().next() {
        Some('A') => "Add",
        Some('D') => "Remove",
        _ => "Update",
    }
}

/// A plain commit message when no model can write one.
pub fn summary_message(files: &[(String, String)], counts: &HashMap<String, (u64, u64)>) -> String {
    let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_owned();
    let verbs: Vec<&str> = files.iter().map(|(s, _)| verb(s)).collect();
    let same = verbs.windows(2).all(|w| w[0] == w[1]);
    let lead = if same { verbs[0] } else { "Update" };
    let subject = match files.len() {
        1 => format!("{lead} {}", files[0].1),
        2 => format!("{lead} {} and {}", name(&files[0].1), name(&files[1].1)),
        n => {
            let dirs: Vec<&str> = files
                .iter()
                .map(|(_, p)| p.rsplit_once('/').map(|(d, _)| d).unwrap_or(""))
                .collect();
            if !dirs[0].is_empty() && dirs.iter().all(|d| *d == dirs[0]) {
                format!("{lead} {n} files in {}", dirs[0])
            } else {
                format!("{lead} {} and {} other files", name(&files[0].1), n - 1)
            }
        }
    };
    let subject = truncate_chars(&subject, 72);
    if files.len() == 1 {
        return subject;
    }
    let lines: Vec<String> = files
        .iter()
        .take(20)
        .map(|(status, path)| {
            let (add, del) = counts.get(path).copied().unwrap_or((0, 0));
            format!("- {} {path} (+{add} -{del})", verb(status))
        })
        .collect();
    let more = if files.len() > 20 {
        format!("\n- and {} more", files.len() - 20)
    } else {
        String::new()
    };
    format!("{subject}\n\n{}{more}", lines.join("\n"))
}

/// A plain pull request title and body when no model can write one.
pub fn summary_pr(branch: &str, commits: &[(String, String)], stat: &str) -> String {
    let title = if commits.len() == 1 {
        commits[0].0.clone()
    } else {
        let words = branch
            .rsplit('/')
            .next()
            .unwrap_or(branch)
            .replace(['-', '_'], " ");
        let mut chars = words.trim().chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => "Update".into(),
        }
    };
    let mut body = String::from("## Changes\n");
    for (subject, _) in commits.iter().take(30) {
        body.push_str(&format!("- {subject}\n"));
    }
    if commits.is_empty() {
        body.push_str("- No commits yet beyond the base branch\n");
    }
    if let Some(total) = stat.lines().last().filter(|l| l.contains("changed")) {
        body.push_str(&format!("\n{}\n", total.trim()));
    }
    format!(
        "Title: {}\n\n{}",
        truncate_chars(&title, 72),
        body.trim_end()
    )
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut cut: String = text.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

/// Drop `<think>` blocks and code fences some models wrap replies in.
pub fn strip_reasoning(text: &str) -> String {
    let mut text = text.to_owned();
    while let Some(start) = text.find("<think>") {
        match text[start..].find("</think>") {
            Some(end) => text.replace_range(start..start + end + "</think>".len(), ""),
            None => text.truncate(start),
        }
    }
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect();
    lines.join("\n").trim().to_owned()
}

/// A commit message with a sane subject line and no stray labels.
pub fn tidy_message(text: &str) -> String {
    let text = text.trim();
    let text = ["Commit message:", "Commit Message:", "Subject:"]
        .iter()
        .find_map(|label| text.strip_prefix(label))
        .unwrap_or(text)
        .trim();
    let mut lines = text.lines();
    let subject = lines
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches(['"', '`', '*'])
        .trim()
        .to_owned();
    let rest: Vec<&str> = lines.collect();
    let body = rest.join("\n").trim().to_owned();
    let subject = truncate_chars(&subject, 100);
    if body.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{body}")
    }
}

/// `Title: …` then the body; without the label the first line is the title.
pub fn split_title(text: &str) -> (String, String) {
    let text = text.trim();
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let title = first
        .trim()
        .strip_prefix("Title:")
        .or_else(|| first.trim().strip_prefix("# "))
        .unwrap_or(first)
        .trim()
        .trim_matches(['"', '`', '*'])
        .trim();
    (truncate_chars(title, 120), rest.trim().to_owned())
}

/// The pull/merge request address in a forge CLI's output.
pub fn find_request_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .rev()
        .find(|word| {
            word.starts_with("https://")
                && (word.contains("/pull/") || word.contains("/merge_requests/"))
        })
        .map(|word| word.trim_end_matches(['.', ',', ')']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_parse_without_credentials() {
        let github = parse_remote("git@github.com:octo/demo.git").unwrap();
        assert_eq!(github.kind, "github");
        assert_eq!(github.path, "octo/demo");
        assert_eq!(github.web_url, "https://github.com/octo/demo");
        let https = parse_remote("https://user:secret-token@github.com/octo/demo").unwrap();
        assert_eq!(https.web_url, "https://github.com/octo/demo");
        assert!(!format!("{https:?}").contains("secret"));
        let lab = parse_remote("ssh://git@gitlab.example.com:2222/group/sub/repo.git").unwrap();
        assert_eq!(lab.kind, "gitlab");
        assert_eq!(lab.path, "group/sub/repo");
        assert_eq!(lab.web_url, "https://gitlab.example.com/group/sub/repo");
        let port = parse_remote("https://git.example.com:8443/team/app.git").unwrap();
        assert_eq!(port.kind, "other");
        assert_eq!(port.web_url, "https://git.example.com:8443/team/app");
        assert!(parse_remote("/srv/git/demo.git").is_none());
        assert!(parse_remote("file:///srv/git/demo.git").is_none());
        assert!(parse_remote("git@github.com:onlyone").is_none());
    }

    #[test]
    fn compare_pages_encode_branch_names() {
        let github = parse_remote("git@github.com:octo/demo.git").unwrap();
        assert_eq!(
            compare_url(&github, "main", "feat/a+b").unwrap(),
            "https://github.com/octo/demo/compare/main...feat/a%2Bb?expand=1"
        );
        let lab = parse_remote("https://gitlab.com/g/r.git").unwrap();
        assert!(compare_url(&lab, "main", "x")
            .unwrap()
            .ends_with("merge_requests/new?merge_request%5Bsource_branch%5D=x&merge_request%5Btarget_branch%5D=main"));
        assert!(compare_url(&parse_remote("https://example.org/a/b").unwrap(), "m", "x").is_none());
    }

    #[test]
    fn branch_names_are_validated() {
        for good in ["feature/login", "fix-12", "user/a.b_c"] {
            assert!(validate_branch(good).is_ok(), "{good}");
        }
        for bad in [
            "", "-x", "a b", "a..b", "a~1", "a^", "a:b", "x.lock", "/x", "x/", ".x", "a/.b",
            "HEAD", "a@{1}", "a\\b", "a//b",
        ] {
            assert!(validate_branch(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn deterministic_messages_describe_the_change() {
        let counts = HashMap::from([("src/a.rs".to_string(), (3, 1))]);
        assert_eq!(
            summary_message(&[("M".into(), "src/a.rs".into())], &counts),
            "Update src/a.rs"
        );
        let many = vec![
            ("A".to_string(), "src/x/a.rs".to_string()),
            ("A".to_string(), "src/x/b.rs".to_string()),
            ("A".to_string(), "src/x/c.rs".to_string()),
        ];
        let text = summary_message(&many, &HashMap::new());
        assert!(text.starts_with("Add 3 files in src/x\n\n- Add src/x/a.rs"));
        let pr = summary_pr(
            "feature/add-login",
            &[("A".into(), String::new()), ("B".into(), String::new())],
            " 2 files changed, 3 insertions(+)",
        );
        let (title, body) = split_title(&pr);
        assert_eq!(title, "Add login");
        assert!(body.contains("- A\n- B"));
        assert!(body.contains("2 files changed"));
    }

    #[test]
    fn model_replies_are_cleaned() {
        assert_eq!(
            tidy_message(&strip_reasoning(
                "<think>hmm</think>```\nCommit message: \"Fix add\"\n\nIt subtracted.\n```"
            )),
            "Fix add\n\nIt subtracted."
        );
        let (title, body) = split_title("Title: Add login\n\nSummary.\n\n## Changes\n- a");
        assert_eq!(title, "Add login");
        assert!(body.starts_with("Summary."));
        assert_eq!(
            find_request_url("Creating pull request\nhttps://github.com/o/r/pull/7\n").unwrap(),
            "https://github.com/o/r/pull/7"
        );
        assert!(find_request_url("https://github.com/o/r").is_none());
    }
}
