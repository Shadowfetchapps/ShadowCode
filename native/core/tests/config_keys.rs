//! `config.example.yaml` documents every setting the code reads, with its
//! real default, and nothing the code ignores; a value that can't be used is
//! reported with its key and how to fix it; settings from older and newer
//! versions are kept.
use serde_json::{json, Value};
use shadowcode_core::{
    code_intel::CodeIntelConfig, config::Config, paths::AppPaths, subagents::SubagentsConfig,
    voice::VoiceConfig,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

const EXAMPLE: &str = include_str!("../../../config.example.yaml");

/// Example values that deliberately differ from the code default, and why.
const DELIBERATE: &[(&str, &str)] = &[(
    "permissions.mode",
    "new installs start in ask mode; without the key, configs from before 0.28 keep their behavior",
)];

fn example() -> Value {
    let value: Value = serde_yaml_ng::from_str(EXAMPLE).expect("config.example.yaml is YAML");
    assert!(value.is_object());
    value
}

/// What the code reads: the typed settings with their defaults, the defaults
/// of the untyped groups, and the groups other modules read from `extra`.
fn code_defaults() -> Value {
    let mut value = serde_json::to_value(Config::default()).unwrap();
    value["voice"] = serde_json::to_value(VoiceConfig::default()).unwrap();
    value["code_intel"] = serde_json::to_value(CodeIntelConfig::default()).unwrap();
    value["subagents"] = serde_json::to_value(SubagentsConfig::default()).unwrap();
    value
}
/// The groups read from `extra` (`config.extra.get("voice")`).
const EXTRA_GROUPS: &[&str] = &["voice", "code_intel", "subagents"];

/// Dotted paths of every setting: objects are groups, anything else
/// (including lists and empty groups) is one setting.
fn leaves(value: &Value) -> BTreeMap<String, Value> {
    fn walk(value: &Value, path: &str, found: &mut BTreeMap<String, Value>) {
        match value {
            Value::Object(map) if !map.is_empty() => {
                for (key, child) in map {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    walk(child, &path, found);
                }
            }
            other => {
                found.insert(path.to_owned(), other.clone());
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(value, "", &mut found);
    found
}

#[test]
fn every_setting_the_code_reads_is_documented_and_nothing_else() {
    let documented = leaves(&example());
    let code = leaves(&code_defaults());
    let missing: Vec<&String> = code
        .keys()
        .filter(|k| !documented.contains_key(*k))
        .collect();
    let unknown: Vec<&String> = documented
        .keys()
        .filter(|k| !code.contains_key(*k))
        .collect();
    assert!(
        missing.is_empty(),
        "settings the code reads but config.example.yaml does not document: {missing:?}"
    );
    assert!(
        unknown.is_empty(),
        "config.example.yaml documents settings no code reads (remove them, or give them a default in Config::default): {unknown:?}"
    );
    assert!(code.len() >= 90, "{}", code.len());
}

#[test]
fn the_example_shows_the_real_defaults() {
    let documented = leaves(&example());
    let code = leaves(&code_defaults());
    for (key, value) in &documented {
        if DELIBERATE.iter().any(|(k, _)| k == key) {
            continue;
        }
        let default = &code[key];
        let same = match (value.as_f64(), default.as_f64()) {
            (Some(a), Some(b)) => a == b,
            _ => value == default,
        };
        assert!(
            same,
            "config.example.yaml says {key}: {value}, but the default is {default}"
        );
    }
}

#[test]
fn the_example_is_a_working_configuration() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    fs::write(paths.config_file(), EXAMPLE).unwrap();
    let config = Config::load(&paths, None).unwrap();
    assert_eq!(
        config.permissions.mode,
        shadowcode_core::config::PermissionMode::Ask
    );
    // The groups other modules read from the same file.
    assert_eq!(
        CodeIntelConfig::from_config(&config).unwrap(),
        CodeIntelConfig::default()
    );
    assert_eq!(
        VoiceConfig::from_config(&config).unwrap(),
        VoiceConfig::default()
    );
    // Saving keeps every documented setting.
    Config::patch(&paths, json!({"ui": {"theme": "dark"}})).unwrap();
    let saved: Value =
        serde_yaml_ng::from_str(&fs::read_to_string(paths.config_file()).unwrap()).unwrap();
    let saved = leaves(&saved);
    for key in leaves(&example()).keys() {
        assert!(saved.contains_key(key), "{key} was dropped on save");
    }
}

/// Keys read by indexing an untyped group (`config.ui["x"]`,
/// `cfg.mcp.get("x")`) must have a default, so they are documented and
/// `shadowcode config KEY VALUE` can set them.
#[test]
fn ad_hoc_reads_of_untyped_groups_have_defaults() {
    let groups = "ui|limits|routing|mcp|onboarding|guardian";
    let access = regex::Regex::new(&format!(
        r#"\b(?:config|cfg|self|updated)\.({groups})(?:\["([a-z_]+)"\]|\.get\("([a-z_]+)"\))"#
    ))
    .unwrap();
    let extra = regex::Regex::new(r#"\.extra\s*\.get\("([a-z_]+)"\)"#).unwrap();
    let mut groups = BTreeSet::new();
    let mut found = BTreeSet::new();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![src.clone(), src.join("../../../src-tauri/src")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                for captures in access.captures_iter(&text) {
                    let key = captures.get(2).or(captures.get(3)).unwrap().as_str();
                    found.insert(format!("{}.{key}", &captures[1]));
                }
                for captures in extra.captures_iter(&text) {
                    groups.insert(captures[1].to_owned());
                }
            }
        }
    }
    // The notification switches are read through one helper.
    let notify = fs::read_to_string(src.join("notify.rs")).unwrap();
    for captures in regex::Regex::new(r#"on\("([a-z_]+)""#)
        .unwrap()
        .captures_iter(&notify)
    {
        found.insert(format!("ui.{}", &captures[1]));
    }
    for purpose in shadowcode_core::routing::PURPOSES {
        found.insert(format!("routing.{purpose}"));
    }
    assert!(found.len() >= 20, "the scan found too little: {found:?}");
    assert!(groups.len() >= 3, "the scan found too little: {groups:?}");
    for group in &groups {
        assert!(
            EXTRA_GROUPS.contains(&group.as_str()),
            "the `{group}` settings group is read from Config::extra: add its defaults to code_defaults and EXTRA_GROUPS in tests/config_keys.rs and document it in config.example.yaml"
        );
    }
    let code = leaves(&code_defaults());
    for key in &found {
        assert!(
            code.contains_key(key),
            "{key} is read but has no default in Config::default (and so is not in config.example.yaml)"
        );
    }
}

fn load_error(yaml: &str) -> String {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    fs::write(paths.config_file(), yaml).unwrap();
    match Config::load(&paths, None) {
        Ok(_) => panic!("{yaml:?} loaded"),
        Err(error) => format!("{error:#}"),
    }
}

#[test]
fn a_value_that_cannot_be_used_names_its_key_and_the_fix() {
    for (yaml, expected) in [
        (
            "agent:\n  max_steps: 0\n",
            &[
                "config.yaml needs a fix",
                "agent.max_steps must be between 1 and 1000",
                "found 0",
            ][..],
        ),
        (
            "agent:\n  max_steps: many\n",
            &["`agent.max_steps`", "can't read", "delete that line"],
        ),
        (
            "permissions:\n  mode: sometimes\n",
            &["`permissions.mode`", "ask", "allow_edits"],
        ),
        (
            "ui:\n  theme: purple\n",
            &["ui.theme must be system, light or dark", "purple"],
        ),
        (
            "model:\n  keep_alive: forever\n",
            &["model.keep_alive", "30m"],
        ),
        (
            "model:\n  context_limit: 10\n",
            &["model.context_limit", "1024"],
        ),
        (
            "model:\n  endpoint: ftp://example.com\n",
            &["model.endpoint", "http://"],
        ),
        (
            "model:\n  endpoint: not a url\n",
            &["model.endpoint", "web address"],
        ),
        (
            "model:\n  api_key_env: sk-123\n",
            &["model.api_key_env", "never the key itself"],
        ),
        (
            "agent:\n  compact_ratio: 2\n",
            &["agent.compact_ratio", "0.2 and 0.95"],
        ),
        (
            "agent:\n  model_retries: 50\n",
            &["agent.model_retries", "0 and 10"],
        ),
        (
            "agent:\n  autonomy_profile: wild\n",
            &["agent.autonomy_profile", "normal"],
        ),
        (
            "guardian:\n  interval_sec: 5\n",
            &["guardian.interval_sec", "60 and 86400"],
        ),
        (
            "limits:\n  on_limit: stop\n",
            &["limits.on_limit must be local or ask"],
        ),
        ("routing:\n  painter: x\n", &["routing.painter", "planner"]),
        (
            "network:\n  allow_local_dev: [nope]\n",
            &["network.allow_local_dev"],
        ),
        ("sandbox:\n  home_binds: [.ssh]\n", &["sandbox.home_binds"]),
        (
            "cli_agents:\n  stall_timeout_sec: 1\n",
            &["cli_agents.stall_timeout_sec"],
        ),
        ("checkpoints:\n  keep: 0\n", &["checkpoints.keep"]),
        (
            "local_engine:\n  context_size: 5\n",
            &["local_engine.context_size"],
        ),
        (
            "local_engine:\n  files: [relative.gguf]\n",
            &["local_engine paths must be absolute"],
        ),
        (
            "verification:\n  commands: ['']\n",
            &["verification.commands"],
        ),
        (
            "hooks:\n  approved: [{workspace: relative, path: .shadow/hooks/x.yaml, hash: x}]\n",
            &["hooks.approved"],
        ),
        ("mcp:\n  servers: {}\n", &["mcp.servers must be a list"]),
        ("- a list\n", &["must be a list of `key: value` settings"]),
        ("agent: [\n", &["Invalid YAML"]),
    ] {
        let error = load_error(yaml);
        for part in expected {
            assert!(error.contains(part), "{yaml:?}: {error:?} lacks {part:?}");
        }
    }
}

#[test]
fn settings_from_other_versions_are_kept_and_ignored() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    // Retired keys (git, logging, ui.host/port/ability, permissions.profile)
    // and a group from a newer version.
    fs::write(
        paths.config_file(),
        "git:\n  auto_commit: true\n  allow_destructive: false\nlogging:\n  level: debug\n\
         ui:\n  host: 127.0.0.1\n  port: 7430\n  ability: none\n  notify_after_sec: 4.0\n  theme: dark\n\
         permissions:\n  profile: ''\n  level: workspace\n\
         from_a_newer_version:\n  enabled: true\n",
    )
    .unwrap();
    let config = Config::load(&paths, None).unwrap();
    assert_eq!(config.ui["theme"], "dark");
    // An old config without permissions.mode keeps its old behavior.
    assert_eq!(
        config.permissions.mode,
        shadowcode_core::config::PermissionMode::AllowEdits
    );
    Config::patch(&paths, json!({"agent": {"max_steps": 20}})).unwrap();
    let saved: Value =
        serde_yaml_ng::from_str(&fs::read_to_string(paths.config_file()).unwrap()).unwrap();
    assert_eq!(saved["git"]["auto_commit"], true);
    assert_eq!(saved["logging"]["level"], "debug");
    assert_eq!(saved["ui"]["host"], "127.0.0.1");
    assert_eq!(saved["from_a_newer_version"]["enabled"], true);
    assert_eq!(saved["agent"]["max_steps"], 20);
}

#[test]
fn a_bad_value_sent_by_settings_names_its_key() {
    use shadowcode_core::service::{Request, Service};
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let service = Service::open(paths, Some(root.path().to_path_buf())).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let error = runtime
        .block_on(service.dispatch(Request {
            method: "PUT".into(),
            path: "/api/config".into(),
            body: json!({"values": {"agent": {"tool_timeout_sec": "slow"}}}),
        }))
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("`agent.tool_timeout_sec`"), "{text}");
    let error = runtime
        .block_on(service.dispatch(Request {
            method: "PUT".into(),
            path: "/api/config".into(),
            body: json!({"values": {"agent": {"tool_timeout_sec": 0}}}),
        }))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("agent.tool_timeout_sec must be between 1 and 3600"),
        "{error:#}"
    );
    runtime.block_on(service.engine.shutdown()).unwrap();
}
