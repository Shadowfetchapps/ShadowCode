//! The GitHub Action under integrations/github-action: its YAML parses, and
//! the safety rules the README promises hold in the action and the example
//! workflows (no expressions pasted into shell code, the API key and the
//! token confined to their own steps, trusted commenters only, no
//! `pull_request_target`, least-privilege permissions, no persisted
//! checkout credentials).
use serde_yaml_ng::Value;
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../integrations/github-action")
}
fn load(path: &str) -> Value {
    let text = fs::read_to_string(root().join(path)).unwrap();
    serde_yaml_ng::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn get<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

#[test]
fn the_action_is_a_composite_with_safe_steps() {
    let action = load("action.yml");
    assert_eq!(text(get(&action, "name")), "ShadowCode");
    assert_eq!(text(get(get(&action, "runs"), "using")), "composite");
    let inputs = get(&action, "inputs").as_mapping().unwrap();
    for (name, input) in inputs {
        assert!(
            !text(get(input, "description")).is_empty(),
            "{name:?} has no description"
        );
    }
    for required in ["task", "api-key"] {
        assert_eq!(get(get(&action, "inputs"), required)["required"], true);
    }
    assert_eq!(
        text(get(get(get(&action, "inputs"), "approval"), "default")),
        "cancel"
    );
    let steps = get(get(&action, "runs"), "steps").as_sequence().unwrap();
    assert!(steps.len() >= 3);
    let mut key_steps = vec![];
    let mut token_steps = vec![];
    for step in steps {
        let name = text(get(step, "name"));
        let script = text(get(step, "run"));
        assert!(
            !script.is_empty(),
            "{name}: composite steps here are scripts"
        );
        assert_eq!(text(get(step, "shell")), "bash", "{name}");
        // Values reach scripts through env, never pasted into shell code.
        assert!(
            !script.contains("${{"),
            "{name} pastes an expression into its script"
        );
        let env = get(step, "env");
        if let Some(env) = env.as_mapping() {
            for (key, value) in env {
                if text(value).contains("inputs.api-key") {
                    key_steps.push((name.to_owned(), text(key).to_owned()));
                }
                if text(value).contains("inputs.github-token") {
                    token_steps.push(name.to_owned());
                }
            }
        }
    }
    assert_eq!(
        key_steps,
        [("Run the task".to_owned(), "SHADOWCODE_API_KEY".to_owned())],
        "only the run step sees the API key"
    );
    assert_eq!(
        token_steps,
        ["Deliver the result"],
        "only delivery sees the token"
    );
    // The download is verified before anything runs.
    let install = steps
        .iter()
        .find(|s| text(get(s, "name")) == "Download and verify ShadowCode")
        .unwrap();
    let script = text(get(install, "run"));
    assert!(script.contains("SHA256SUMS") && script.contains("sha256sum"));
    assert!(script.contains("--proto '=https'"));
    assert!(script.find("Checksum mismatch").unwrap() < script.find("--appimage-extract").unwrap());
    let run = steps
        .iter()
        .find(|s| text(get(s, "name")) == "Run the task")
        .unwrap();
    assert!(text(get(run, "run")).contains("run --json --approval \"$APPROVAL\""));
}

#[test]
fn the_spending_limit_can_be_raised_and_is_explained() {
    // A fresh profile limits each task on a paid model to $1, and the CLI's
    // advice (--max-cost, --interactive) cannot be followed in a workflow.
    let action = load("action.yml");
    let input = get(get(&action, "inputs"), "max-cost");
    assert_eq!(input["required"], false);
    assert_eq!(text(get(input, "default")), "");
    let steps = get(get(&action, "runs"), "steps").as_sequence().unwrap();
    let step = |name: &str| {
        steps
            .iter()
            .find(|s| text(get(s, "name")) == name)
            .unwrap_or_else(|| panic!("{name}"))
    };
    let run = step("Run the task");
    assert_eq!(
        text(get(get(run, "env"), "MAX_COST")),
        "${{ inputs.max-cost }}"
    );
    let script = text(get(run, "run"));
    assert!(script.contains("args+=(--max-cost \"$MAX_COST\")"));
    // The daily limit must not stop a larger task first.
    assert!(script.contains("sc config spending.daily_usd \"$MAX_COST\""));
    assert!(text(get(step("Check inputs"), "run")).contains("max-cost must be"));
    let deliver = text(get(step("Deliver the result"), "run"));
    assert!(deliver.contains("spending_limit)"));
    assert!(deliver.contains("`max-cost`"));
    assert!(text(get(step("Report the outcome"), "run")).contains("spending_limit"));
    let exit_code = text(get(
        get(get(&action, "outputs"), "exit-code"),
        "description",
    ));
    assert!(exit_code.contains("spending limit"), "{exit_code}");
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    assert!(readme.contains("| `max-cost`"));
    assert!(readme.contains("2 stopped for approval or at the spending\nlimit"));
}

fn triggers(workflow: &Value) -> Value {
    // YAML 1.1 readers turn `on` into `true`; accept both spellings.
    workflow
        .get("on")
        .or_else(|| workflow.get(Value::Bool(true)))
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn example_workflows_are_safe_by_default() {
    let mut names = vec![];
    for entry in fs::read_dir(root().join("examples")).unwrap() {
        let path = entry.unwrap().path();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let raw = fs::read_to_string(&path).unwrap();
        let workflow: Value =
            serde_yaml_ng::from_str(&raw).unwrap_or_else(|e| panic!("{file}: {e}"));
        names.push(file.clone());
        assert!(!raw.contains("write-all"), "{file}");
        let on = triggers(&workflow);
        assert!(on.is_mapping(), "{file}: triggers");
        // Never with a stranger's code and your secrets.
        assert!(on.get("pull_request_target").is_none(), "{file}");
        assert!(on.get("workflow_run").is_none(), "{file}");
        // Nothing by default; each job asks for what it needs.
        assert_eq!(
            get(&workflow, "permissions").as_mapping().map(|m| m.len()),
            Some(0),
            "{file}: top-level permissions must be empty"
        );
        for (job, spec) in get(&workflow, "jobs").as_mapping().unwrap() {
            let perms = get(spec, "permissions")
                .as_mapping()
                .unwrap_or_else(|| panic!("{file} {job:?}: permissions"));
            for (scope, level) in perms {
                assert!(
                    matches!(text(scope), "contents" | "issues" | "pull-requests"),
                    "{file}: unexpected permission {scope:?}"
                );
                assert!(matches!(text(level), "read" | "write"));
            }
            assert!(
                get(spec, "timeout-minutes").as_u64().is_some(),
                "{file}: timeout"
            );
            let steps = get(spec, "steps").as_sequence().unwrap();
            let checkout = steps
                .iter()
                .find(|s| text(get(s, "uses")).starts_with("actions/checkout@"))
                .unwrap_or_else(|| panic!("{file}: checkout"));
            assert_eq!(
                get(get(checkout, "with"), "persist-credentials"),
                &Value::Bool(false),
                "{file}"
            );
            let action = steps
                .iter()
                .find(|s| text(get(s, "uses")).contains("integrations/github-action@"))
                .unwrap_or_else(|| panic!("{file}: uses the action"));
            let with = get(action, "with");
            assert!(
                text(get(with, "api-key")).contains("secrets."),
                "{file}: key from secrets"
            );
            for step in steps {
                assert!(
                    !text(get(step, "run")).contains("${{"),
                    "{file}: expression in a script"
                );
            }
            let condition = text(get(spec, "if"));
            if on.get("issue_comment").is_some() {
                for role in ["OWNER", "MEMBER", "COLLABORATOR"] {
                    assert!(condition.contains(role), "{file}: trusted commenters only");
                }
                assert!(condition.contains("author_association"));
                assert!(condition.contains("github.event.issue.pull_request == null"));
            }
            if on.get("pull_request").is_some() {
                assert!(
                    condition.contains("head.repo.full_name == github.repository"),
                    "{file}: forks are skipped"
                );
                assert_eq!(text(get(with, "mode")), "ask");
            }
        }
    }
    names.sort();
    assert_eq!(names, ["issue-comment.yml", "pr-label.yml"]);
}
