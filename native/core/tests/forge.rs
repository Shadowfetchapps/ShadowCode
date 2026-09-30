//! The drawer's Git panel end to end: branch, suggested messages, commit,
//! push to a local bare remote, and a pull request through a fake `gh` on
//! PATH (no network, no real GitHub).
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::Duration,
};

/// A fake `gh` that answers from files in the repository's `.git` folder and
/// logs every pull request it creates.
const FAKE_GH: &str = r#"#!/bin/sh
state="$(git rev-parse --git-dir 2>/dev/null)"
case "$1" in
  --version) echo "gh version 2.99.0 (fake)"; exit 0 ;;
esac
case "$1 $2" in
  "auth status")
    if [ -f "$state/fake-gh-signed-out" ]; then
      echo "You are not logged into any GitHub hosts. To log in, run: gh auth login" >&2
      exit 1
    fi
    echo "github.com"
    echo "  ✓ Logged in to github.com account octo (keyring)"
    echo "  - Token: gho_************************************"
    exit 0 ;;
  "pr create")
    printf '%s\n' "$@" >> "$state/fake-gh.log"
    echo "Creating pull request for feature/login into main in octo/demo"
    echo "https://github.com/octo/demo/pull/7"
    exit 0 ;;
  "pr view")
    if [ -f "$state/fake-gh-pr" ]; then cat "$state/fake-gh-pr"; exit 0; fi
    echo "no pull requests found for branch" >&2
    exit 1 ;;
  "pr checks")
    echo '[{"name":"build","state":"SUCCESS","bucket":"pass","link":"https://github.com/octo/demo/actions/runs/1","workflow":"CI","description":""},{"name":"test","state":"IN_PROGRESS","bucket":"pending","link":"https://github.com/octo/demo/actions/runs/2","workflow":"CI","description":""}]'
    exit 8 ;;
esac
echo "fake gh: unsupported $*" >&2
exit 2
"#;

fn fake_gh_on_path() {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("shadowcode-fake-gh-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        fs::write(&gh, FAKE_GH).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", dir.display()));
        dir
    });
}

fn git(dir: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).trim().to_owned()
}

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    remote: PathBuf,
    service: Service,
}

/// A project on `main` whose `origin` reads as GitHub but pushes to a local
/// bare repository.
fn setup(model_endpoint: &str) -> Fixture {
    fake_gh_on_path();
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let remote = root.path().join("remote.git");
    fs::create_dir(&project).unwrap();
    git(
        root.path(),
        &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
    );
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.name", "Panel Test"]);
    git(&project, &["config", "user.email", "panel@example.invalid"]);
    fs::write(project.join("README.md"), "# Demo\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "Initial commit"]);
    git(
        &project,
        &["remote", "add", "origin", "git@github.com:octo/demo.git"],
    );
    git(
        &project,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            remote.to_str().unwrap(),
        ],
    );
    git(&project, &["push", "-u", "origin", "main"]);
    // The fetch URL is GitHub's, so seed the remote-tracking ref by hand.
    git(
        &project,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project],"model":{"provider":"local","name":"drafter","endpoint":model_endpoint,"context_limit":8192},"permissions":{"approve_shell":false},"agent":{"retry_attempts":0}})).unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    Fixture {
        _root: root,
        project,
        remote,
        service,
    }
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_commit_push_and_pull_request_with_checks() {
    let f = setup("http://127.0.0.1:9/v1");
    let s = &f.service;
    let overview = call(s, "GET", "/api/git", Value::Null).await.unwrap();
    assert_eq!(overview["repo"], true);
    assert_eq!(overview["branch"], "main");
    assert_eq!(overview["upstream"], "origin/main");
    assert_eq!(overview["remote"], "origin");
    assert_eq!(overview["remote_info"]["kind"], "github");
    assert_eq!(
        overview["remote_info"]["web_url"],
        "https://github.com/octo/demo"
    );
    assert_eq!(overview["default_base"], "main");

    // Branch names are checked before Git sees them.
    for bad in ["bad name", "-rf", "a..b", "x.lock"] {
        let error = call(
            s,
            "POST",
            "/api/git/branch",
            json!({"name": bad, "create": true}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("branch"), "{bad}: {error}");
    }
    call(
        s,
        "POST",
        "/api/git/branch",
        json!({"name": "feature/login", "create": true}),
    )
    .await
    .unwrap();
    let overview = call(s, "GET", "/api/git", Value::Null).await.unwrap();
    assert_eq!(overview["branch"], "feature/login");
    assert_eq!(overview["upstream"], Value::Null);

    // Nothing staged: no message to suggest.
    let error = call(s, "POST", "/api/git/suggest", json!({"kind": "commit"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("Nothing is staged"));
    fs::write(f.project.join("login.txt"), "sign in\n").unwrap();
    fs::write(f.project.join(".env"), "API_KEY=do-not-send\n").unwrap();
    call(
        s,
        "POST",
        "/api/workspace/git/add",
        json!({"paths": ["login.txt"]}),
    )
    .await
    .unwrap();
    // The configured model is unreachable, so a plain summary is offered.
    let draft = call(s, "POST", "/api/git/suggest", json!({"kind": "commit"}))
        .await
        .unwrap();
    assert_eq!(draft["source"], "summary");
    assert_eq!(draft["message"], "Add login.txt");
    assert!(draft["note"].as_str().unwrap().contains("plain summary"));
    call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message": draft["message"]}),
    )
    .await
    .unwrap();

    let pushed = call(s, "POST", "/api/git/push", json!({})).await.unwrap();
    assert_eq!(pushed["branch"], "feature/login");
    assert_eq!(pushed["remote"], "origin");
    assert_eq!(
        git(&f.remote, &["rev-parse", "refs/heads/feature/login"]),
        git(&f.project, &["rev-parse", "HEAD"])
    );
    let overview = call(s, "GET", "/api/git", Value::Null).await.unwrap();
    assert_eq!(overview["upstream"], "origin/feature/login");
    assert_eq!(overview["ahead"], 0);

    let status = call(s, "GET", "/api/git/pr?base=main", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["provider"], "github");
    assert_eq!(status["cli"]["name"], "gh");
    assert_eq!(status["cli"]["installed"], true);
    assert_eq!(status["cli"]["authenticated"], true);
    assert!(status["cli"]["detail"]
        .as_str()
        .unwrap()
        .contains("Logged in"));
    assert!(!status.to_string().contains("gho_"));
    assert_eq!(
        status["compare_url"],
        "https://github.com/octo/demo/compare/main...feature/login?expand=1"
    );
    assert_eq!(status["pr"], Value::Null);

    let pr_draft = call(
        s,
        "POST",
        "/api/git/suggest",
        json!({"kind": "pr", "base": "main"}),
    )
    .await
    .unwrap();
    assert_eq!(pr_draft["title"], "Add login.txt");
    assert!(pr_draft["body"]
        .as_str()
        .unwrap()
        .contains("- Add login.txt"));

    // An unpushed commit is pushed before the pull request opens.
    fs::write(f.project.join("login.txt"), "sign in\nsign out\n").unwrap();
    call(
        s,
        "POST",
        "/api/workspace/git/add",
        json!({"paths": ["login.txt"]}),
    )
    .await
    .unwrap();
    call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message": "Add sign out"}),
    )
    .await
    .unwrap();
    let created = call(
        s,
        "POST",
        "/api/git/pr",
        json!({"title": "Add login", "body": "Adds the login page.", "base": "main", "draft": true}),
    )
    .await
    .unwrap();
    assert_eq!(created["url"], "https://github.com/octo/demo/pull/7");
    assert_eq!(created["number"], 7);
    assert_eq!(created["pushed"], true);
    assert_eq!(
        git(&f.remote, &["rev-parse", "refs/heads/feature/login"]),
        git(&f.project, &["rev-parse", "HEAD"])
    );
    let logged = fs::read_to_string(f.project.join(".git/fake-gh.log")).unwrap();
    let args: Vec<&str> = logged.lines().collect();
    for pair in [
        ["--repo", "octo/demo"],
        ["--base", "main"],
        ["--head", "feature/login"],
        ["--title", "Add login"],
        ["--body", "Adds the login page."],
    ] {
        assert!(args.windows(2).any(|w| w == pair), "{pair:?} in {args:?}");
    }
    assert!(args.contains(&"--draft"));

    let checks = call(s, "GET", "/api/git/pr/checks?number=7", Value::Null)
        .await
        .unwrap();
    assert_eq!(checks["overall"], "pending");
    assert_eq!(checks["summary"]["pass"], 1);
    assert_eq!(checks["summary"]["pending"], 1);
    assert_eq!(checks["checks"][0]["name"], "build");
    assert_eq!(checks["url"], "https://github.com/octo/demo/pull/7/checks");
    assert!(
        call(s, "GET", "/api/git/pr/checks?number=7;rm", Value::Null)
            .await
            .is_err()
    );

    // An existing pull request is found on the next look.
    fs::write(
        f.project.join(".git/fake-gh-pr"),
        r#"{"number":7,"url":"https://github.com/octo/demo/pull/7","state":"OPEN","isDraft":true,"title":"Add login","baseRefName":"main","headRefName":"feature/login"}"#,
    )
    .unwrap();
    let status = call(s, "GET", "/api/git/pr", Value::Null).await.unwrap();
    assert_eq!(status["pr"]["number"], 7);
    assert_eq!(status["pr"]["draft"], true);

    // Signed out: the panel explains how to sign in and still offers the
    // compare page; creating refuses.
    fs::write(f.project.join(".git/fake-gh-signed-out"), "").unwrap();
    let status = call(s, "GET", "/api/git/pr", Value::Null).await.unwrap();
    assert_eq!(status["cli"]["authenticated"], false);
    assert_eq!(
        status["cli"]["login_command"],
        "gh auth login --hostname github.com"
    );
    assert!(status["compare_url"]
        .as_str()
        .unwrap()
        .contains("/compare/"));
    let error = call(
        s,
        "POST",
        "/api/git/pr",
        json!({"title": "Again", "body": "", "base": "main"}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("not ready"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn suggestions_use_the_conversation_model_and_never_send_secret_files() {
    let server = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"role":"assistant","content":"<think>look at the diff</think>\nAdd the login page\n\nUsers can now sign in."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
            Duration::ZERO,
        )
    })
    .await;
    let f = setup(&server.endpoint);
    let s = &f.service;
    fs::write(f.project.join("login.txt"), "sign in\n").unwrap();
    fs::write(f.project.join(".env"), "API_KEY=do-not-send\n").unwrap();
    call(
        s,
        "POST",
        "/api/workspace/git/add",
        json!({"paths": ["login.txt", ".env"]}),
    )
    .await
    .unwrap();
    let draft = call(s, "POST", "/api/git/suggest", json!({"kind": "commit"}))
        .await
        .unwrap();
    assert_eq!(draft["source"], "model");
    assert_eq!(draft["model"], "drafter");
    assert_eq!(
        draft["message"],
        "Add the login page\n\nUsers can now sign in."
    );
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    let sent = requests[0].to_string();
    assert!(sent.contains("login.txt") && sent.contains("sign in"));
    // The secret file is named in the list but its contents never leave.
    assert!(!sent.contains("do-not-send"));
}

#[tokio::test(flavor = "multi_thread")]
async fn paid_drafts_count_toward_today_and_stop_at_the_daily_limit() {
    let server = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"role":"assistant","content":"Add the login page"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15,"cost":0.03}}),
            Duration::ZERO,
        )
    })
    .await;
    let f = setup(&server.endpoint);
    let s = &f.service;
    // The conversation's model is billed per token.
    Config::patch(
        s.engine.paths(),
        json!({
            "model":{"provider":"openrouter","name":"acme/drafter","endpoint":server.endpoint,"api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":8192},
            "spending":{"daily_usd":0.05},
        }),
    )
    .unwrap();
    fs::write(f.project.join("login.txt"), "sign in\n").unwrap();
    call(
        s,
        "POST",
        "/api/workspace/git/add",
        json!({"paths": ["login.txt"]}),
    )
    .await
    .unwrap();
    let today = || {
        shadowcode_core::spending::today(&s.engine.store(), shadowcode_core::now())
            .unwrap()
            .usd
    };
    for spent in [0.03, 0.06] {
        let draft = call(s, "POST", "/api/git/suggest", json!({"kind": "commit"}))
            .await
            .unwrap();
        assert_eq!(draft["source"], "model", "{draft}");
        assert!((today() - spent).abs() < 1e-9, "{}", today());
    }
    // Today's limit is reached: a plain summary, and nothing is sent.
    let draft = call(s, "POST", "/api/git/suggest", json!({"kind": "commit"}))
        .await
        .unwrap();
    assert_eq!(draft["source"], "summary", "{draft}");
    assert!(
        draft["note"]
            .as_str()
            .unwrap()
            .contains("Today's spending limit for paid models ($0.05) is reached"),
        "{draft}"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn push_without_a_remote_explains_itself() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("solo");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-b", "main"]);
    git(&project, &["config", "user.name", "Solo"]);
    git(&project, &["config", "user.email", "solo@example.invalid"]);
    fs::write(project.join("a.txt"), "a\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-m", "One"]);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let overview = call(&service, "GET", "/api/git", Value::Null)
        .await
        .unwrap();
    assert_eq!(overview["remote"], Value::Null);
    assert_eq!(overview["ahead"], 1);
    let error = call(&service, "POST", "/api/git/push", json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no remote"), "{error}");
    let status = call(&service, "GET", "/api/git/pr", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["provider"], Value::Null);
    // Not a repository at all.
    let plain = root.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let other = service.fork_selection(plain, None).unwrap();
    assert_eq!(
        call(&other, "GET", "/api/git", Value::Null).await.unwrap()["repo"],
        false
    );
}

/// A GitHub-token-shaped placeholder, built at runtime so the repository's
/// own secret scan never sees a token-shaped literal.
fn fake_token() -> String {
    format!("ghp_{}", "aB3dE5fG7hI9jK1lM3nO5pQ7rS9tU1vW3xY5")
}

#[tokio::test(flavor = "multi_thread")]
async fn secrets_are_caught_before_commit_and_push_and_hooks_are_asked_once() {
    let f = setup("http://127.0.0.1:9/v1");
    let s = &f.service;
    let commits = |project: &Path| git(project, &["rev-list", "--count", "HEAD"]);
    let before = commits(&f.project);

    // A staged token stops the commit; nothing is committed.
    fs::write(
        f.project.join("config.py"),
        format!("TOKEN = \"{}\"\n", fake_token()),
    )
    .unwrap();
    git(&f.project, &["add", "config.py"]);
    let refused = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Add config"}),
    )
    .await
    .unwrap();
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["status"], 409);
    assert_eq!(refused["secrets"][0]["path"], "config.py");
    assert_eq!(refused["secrets"][0]["line"], 1);
    assert_eq!(refused["secrets"][0]["kind"], "a GitHub token");
    assert!(!refused.to_string().contains(&fake_token()));
    assert_eq!(commits(&f.project), before);

    // "Add to .gitignore" keeps the file local and out of the commit.
    call(
        s,
        "POST",
        "/api/workspace/git/ignore",
        json!({"path":"config.py"}),
    )
    .await
    .unwrap();
    assert!(fs::read_to_string(f.project.join(".gitignore"))
        .unwrap()
        .contains("/config.py"));
    assert_eq!(git(&f.project, &["diff", "--cached", "--name-only"]), "");

    // "Commit anyway" commits an environment file the user chose to keep.
    fs::write(f.project.join(".env.local"), "PORT=3000\n").unwrap();
    git(&f.project, &["add", ".env.local", ".gitignore"]);
    let again = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Local env"}),
    )
    .await
    .unwrap();
    assert_eq!(
        again["secrets"][0]["kind"], "an environment file",
        "{again}"
    );
    let done = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Local env","allow_secrets":true}),
    )
    .await
    .unwrap();
    assert_eq!(done["ok"], true, "{done}");

    // The push sees it too, until the user pushes anyway.
    let push = call(s, "POST", "/api/git/push", json!({})).await.unwrap();
    assert_eq!(push["ok"], false, "{push}");
    assert_eq!(push["secrets"][0]["path"], ".env.local");
    assert!(push["secrets"][0]["commit"].as_str().is_some());
    let pushed = call(s, "POST", "/api/git/push", json!({"allow_secrets":true}))
        .await
        .unwrap();
    assert_eq!(pushed["ok"], true, "{pushed}");

    // The project's own hooks: asked once, remembered, changeable.
    let hook = f.project.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\ntouch hook-ran\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let stage = |text: &str| {
        fs::write(f.project.join("README.md"), text).unwrap();
        git(&f.project, &["add", "README.md"]);
    };
    stage("# Demo\n\nOne\n");
    let ask = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"One"}),
    )
    .await
    .unwrap();
    assert_eq!(ask["needs_hooks_choice"], true, "{ask}");
    assert_eq!(ask["hooks"][0]["name"], "pre-commit");
    assert_eq!(ask["hooks"][0]["preview"], "touch hook-ran");
    let skipped = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"One","hooks":"skip"}),
    )
    .await
    .unwrap();
    assert_eq!(skipped["ok"], true, "{skipped}");
    assert!(!f.project.join("hook-ran").exists());
    stage("# Demo\n\nTwo\n");
    let quiet = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Two"}),
    )
    .await
    .unwrap();
    assert_eq!(quiet["ok"], true, "{quiet}");
    assert!(!f.project.join("hook-ran").exists());
    let hooks = call(s, "POST", "/api/workspace/git/hooks", json!({"run":true}))
        .await
        .unwrap();
    assert_eq!(hooks["run"], true);
    stage("# Demo\n\nThree\n");
    let ran = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Three"}),
    )
    .await
    .unwrap();
    assert_eq!(ran["ok"], true, "{ran}");
    assert_eq!(ran["hooks_ran"], true);
    assert!(f.project.join("hook-ran").exists());
    // A failing hook stops the commit and says why.
    fs::write(
        &hook,
        "#!/bin/sh\necho 'lint failed: 2 problems' >&2\nexit 1\n",
    )
    .unwrap();
    stage("# Demo\n\nFour\n");
    let stopped = call(
        s,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Four"}),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{stopped:#}").contains("lint failed"),
        "{stopped:#}"
    );
    assert!(format!("{stopped:#}").contains("hooks stopped the commit"));
}
