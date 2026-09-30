//! A deliberately small support snapshot. Doctor's full display response is
//! never reused as export content; only reviewed identities and statuses cross.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_CHECKS: usize = 96;
const MAX_BYTES: usize = 256 * 1024;
/// The app log's last lines in an export, at most this much.
pub(super) const LOG_BYTES: usize = 96 * 1024;
/// Run records of the latest jobs in an export.
pub(super) const RUNS: usize = 12;

/// A log line without paths: anything that looks like a file path is
/// replaced (the log already has secrets redacted and home as `~`).
fn scrub_paths(line: &str) -> String {
    static PATH: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = PATH.get_or_init(|| {
        regex::Regex::new(r#"(?:~|\.{1,2})?/[^\s"',;:()\[\]]*/[^\s"',;:()\[\]]*"#)
            .expect("path regex")
    });
    re.replace_all(line, "<path>").into_owned()
}

/// Public model ids (OpenRouter and vendor CLIs); other ids and names can
/// name local files, private hosts or a custom model.
fn public_model(id: &str) -> bool {
    id.starts_with(crate::openrouter::ID_PREFIX) || id.starts_with("cli:")
}

/// A log line without private model names: its `model`, `model_id` and
/// `target` fields keep only public ids ([`public_model`]) and a
/// subscription's name (a vendor task's `model`). The log itself keeps them
/// for the user; lines written by any version pass through here.
fn scrub_models(line: &str) -> String {
    static FIELD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = FIELD.get_or_init(|| {
        regex::Regex::new(r#"\b(model|model_id|target)="((?:[^"\\]|\\.)*)""#)
            .expect("model field regex")
    });
    let vendor = |name: &str| {
        crate::cli_agent::Vendor::ALL
            .iter()
            .any(|v| name == v.label() || name == v.product_label())
    };
    re.replace_all(line, |caps: &regex::Captures| {
        let (field, value) = (&caps[1], &caps[2]);
        if public_model(value) || (field == "model" && vendor(value)) {
            caps[0].to_owned()
        } else {
            format!("{field}=\"(hidden)\"")
        }
    })
    .into_owned()
}

/// A run record as it may leave the computer: public model ids (OpenRouter
/// and vendor CLIs) stay; other ids can name local files or private hosts.
fn public_run(run: &Value) -> Value {
    let id = run["model_id"].as_str().unwrap_or("");
    let public = public_model(id);
    let mut out = serde_json::Map::new();
    for key in [
        "provider",
        "route",
        "vendor",
        "vendor_version",
        "effort",
        "app_version",
        "app_commit",
        "settings_hash",
        "rules_hash",
        "recorded_at",
    ] {
        out.insert(key.into(), run[key].clone());
    }
    out.insert(
        "model_id".into(),
        json!(if public {
            id
        } else if id.starts_with("local:gguf:") {
            "local:gguf:(hidden)"
        } else {
            "(custom model, hidden)"
        }),
    );
    if public {
        out.insert("model".into(), run["model"].clone());
    }
    Value::Object(out)
}
const TTL: Duration = Duration::from_secs(10 * 60);

struct Snapshot {
    id: String,
    created: Instant,
    content: String,
    captured_at: String,
}

#[derive(Default)]
pub(super) struct DiagnosticExports(Mutex<VecDeque<Snapshot>>);

fn label(id: &str) -> Option<&'static str> {
    Some(match id {
        "runtime" => "Native runtime",
        "config" => "Configuration",
        "config-directory" => "Private configuration directory",
        "data-directory" => "Private data directory",
        "state-directory" => "Private state directory",
        "secrets-permissions" => "Credential file permissions",
        "database" => "History database integrity",
        "git" => "Git executable",
        "project-scan" => "Project inspection",
        "project-docs" => "Project documentation",
        "model-configured" => "Coding model selected",
        "model-response" => "Actual model response",
        "history-scale" => "Local history size",
        "autonomy-budget" => "Autonomy budget",
        "rules-and-skills" => "Rules and skills",
        "provider-profile" => "Provider capability profile",
        "shell-policy" => "Shell policy",
        "bubblewrap" => "Bubblewrap availability",
        "landlock" => "Landlock availability",
        "workspace-cow" => "Live workspace writes",
        _ => return None,
    })
}

/// `extra`: `runs` (recent jobs: `{status, run}`) and `log` (the app log's
/// last lines, already redacted).
fn project(report: &Value, extra: &Value, captured_at: &str) -> Result<String> {
    let checks = report["checks"]
        .as_array()
        .context("Doctor checks missing")?;
    let mut selected = Vec::new();
    let mut omitted = checks.len().saturating_sub(MAX_CHECKS);
    for check in checks.iter().take(MAX_CHECKS) {
        let Some(id) = check["id"].as_str() else {
            omitted += 1;
            continue;
        };
        let Some(label) = label(id) else {
            omitted += 1;
            continue;
        };
        let Some(status) = check["status"]
            .as_str()
            .filter(|s| matches!(*s, "pass" | "warn" | "fail" | "info" | "not_checked"))
        else {
            omitted += 1;
            continue;
        };
        // Labels, details, fixes and all other doctor fields may include local
        // paths, configured names or provider output. None are forwarded.
        selected.push(json!({"id":id,"label":label,"status":status}));
    }
    let value = json!({
        "schema": 1,
        "captured_at": captured_at,
        "app": "ShadowCode",
        "version": crate::VERSION,
        "runtime": "rust",
        "os": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "scope": "Completed local Doctor check statuses only; this is not model, package or full-system qualification.",
        "excluded": ["credentials", "paths", "project names and source", "custom and local model names", "prompts, answers and file contents", "crash reports"],
        "omitted_checks": omitted,
        "checks": selected,
        "runs": extra["runs"].as_array().into_iter().flatten().take(RUNS).filter(|job| job["run"].is_object()).map(|job| json!({
            "status": job["status"].as_str().unwrap_or(""),
            "run": public_run(&job["run"]),
        })).collect::<Vec<_>>(),
        "log": {
            "note": "The app log's last lines: events, errors and timings only, with secrets, paths and private model names removed.",
            "lines": extra["log"].as_array().into_iter().flatten().filter_map(Value::as_str).map(|line| scrub_paths(&crate::redaction::redact_text(&scrub_models(line)).text)).collect::<Vec<_>>(),
        },
    });
    let content = format!("{}\n", serde_json::to_string_pretty(&value)?);
    ensure!(
        content.len() <= MAX_BYTES,
        "Diagnostic export exceeds size limit"
    );
    Ok(content)
}

impl DiagnosticExports {
    pub(super) fn prepare(&self, report: &Value, extra: &Value) -> Result<Value> {
        let captured_at = chrono::Utc::now().to_rfc3339();
        let content = project(report, extra, &captured_at)?;
        let id = crate::id();
        let mut snapshots = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Diagnostic export cache poisoned"))?;
        snapshots.retain(|s| s.created.elapsed() < TTL);
        if snapshots.len() == 4 {
            snapshots.pop_front();
        }
        snapshots.push_back(Snapshot {
            id: id.clone(),
            created: Instant::now(),
            content: content.clone(),
            captured_at: captured_at.clone(),
        });
        Ok(
            json!({"id":id,"filename":"shadowcode-diagnostics.json","content":content,"mime":"application/json","captured_at":captured_at,"byte_length":content.len()}),
        )
    }

    pub(super) fn get(&self, id: &str) -> Result<Value> {
        ensure!(
            id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid diagnostic snapshot ID"
        );
        let mut snapshots = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Diagnostic export cache poisoned"))?;
        snapshots.retain(|s| s.created.elapsed() < TTL);
        let snapshot = snapshots
            .iter()
            .find(|s| s.id == id)
            .context("Diagnostic snapshot expired; run Doctor again")?;
        Ok(
            json!({"id":snapshot.id,"filename":"shadowcode-diagnostics.json","content":snapshot.content,"mime":"application/json","captured_at":snapshot.captured_at,"byte_length":snapshot.content.len()}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_keeps_status_without_doctor_secrets_or_paths() {
        let secret = "ghp_123456789012345678901234567890";
        let report = json!({"checks":[
            {"id":"database","status":"fail","label":secret,"detail":format!("/home/alice {secret}"),"fix":secret},
            {"id":"model-response","status":"not_checked","detail":secret},
            {"id":"future-secret-check","status":"pass","detail":secret}
        ],"project_map":{"name":secret},"config":{"key":secret}});
        let content = project(&report, &Value::Null, "2026-01-01T00:00:00Z").unwrap();
        assert!(!content.contains(secret));
        assert!(!content.contains("/home/alice"));
        assert_eq!(
            serde_json::from_str::<Value>(&content).unwrap()["checks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            serde_json::from_str::<Value>(&content).unwrap()["checks"][0]["status"],
            "fail"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&content).unwrap()["checks"][1]["status"],
            "not_checked"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&content).unwrap()["omitted_checks"],
            1
        );
    }

    #[test]
    fn runs_and_log_lines_are_included_without_paths_or_private_models() {
        // Built at runtime: no key-shaped literal in the source.
        let secret = &format!("sk-or-v1-{}", "0123456789abcdef".repeat(3));
        let extra = json!({
            "runs": [
                {"status":"completed","run":{"model_id":"api:openrouter:qwen/qwen3-coder","model":"qwen/qwen3-coder","provider":"openrouter","route":"native_http","effort":"high","app_version":"1.0.0","settings_hash":"abc123abc123","rules_hash":"def456def456"}},
                {"status":"failed","run":{"model_id":"local:gguf:/home/ada/models/secret-name.gguf","model":"secret-name","provider":"llamacpp","vendor":null}},
                {"status":"completed","run":{"model_id":"cli:codex","model":"gpt-5","vendor":"Codex","vendor_version":"codex-cli 0.158.0"}},
                {"status":"completed"}
            ],
            "log": [
                "2026-09-29T10:00:00.000+02:00 INFO  event: tool.completed task=\"t\" tool=\"read_file\" success=true",
                format!("2026-09-29T10:00:01.000+02:00 WARN  engine: could not read ~/work/app/.env with {secret}"),
                "2026-09-29T10:00:02.000+02:00 ERROR engine: failed in /opt/project/src/main.rs",
                "2026-09-29T10:00:03.000+02:00 INFO  event: agent.started task=\"t\" job_id=\"j\" mode=\"code\" model=\"acme-internal-coder\" native=true",
                "2026-09-29T10:00:03.000+02:00 INFO  event: routing.selected task=\"t\" model_id=\"local:gguf:acme-7b\" provider=\"llamacpp\" route=\"local_llamacpp\"",
                "2026-09-29T10:00:04.000+02:00 INFO  event: spend.unknown task=\"t\" model=\"acme-internal-coder\"",
                "2026-09-29T10:00:05.000+02:00 INFO  event: routing.selected task=\"t\" model_id=\"api:openrouter:qwen/qwen3-coder\" provider=\"openrouter\"",
                "2026-09-29T10:00:06.000+02:00 INFO  event: agent.started task=\"t\" job_id=\"j\" mode=\"code\" model=\"Codex (vendor agent)\" native=false vendor_agent=\"codex\"",
                "2026-09-29T10:00:07.000+02:00 INFO  event: resume.scheduled task=\"t\" at=1790000000 target=\"cli:codex\""
            ]
        });
        let content = project(&json!({"checks":[]}), &extra, "2026-01-01T00:00:00Z").unwrap();
        let value: Value = serde_json::from_str(&content).unwrap();
        let runs = value["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(
            runs[0]["run"]["model_id"],
            "api:openrouter:qwen/qwen3-coder"
        );
        assert_eq!(runs[0]["run"]["settings_hash"], "abc123abc123");
        assert_eq!(runs[1]["run"]["model_id"], "local:gguf:(hidden)");
        assert_eq!(runs[1]["run"]["model"], Value::Null);
        assert_eq!(runs[2]["run"]["vendor_version"], "codex-cli 0.158.0");
        let lines = value["log"]["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 9);
        assert!(lines[0].as_str().unwrap().contains("tool=\"read_file\""));
        for private in [
            secret,
            "secret-name",
            "/opt/project",
            "~/work",
            ".env",
            "acme-internal-coder",
            "acme-7b",
        ] {
            assert!(!content.contains(private), "{private}");
        }
        assert!(lines[2].as_str().unwrap().contains("failed in <path>"));
        let line = |i: usize| lines[i].as_str().unwrap();
        assert!(
            line(3).contains(" model=\"(hidden)\" native=true"),
            "{}",
            line(3)
        );
        assert!(line(4).contains("model_id=\"(hidden)\" provider=\"llamacpp\""));
        // Public ids and a subscription's product name stay.
        assert!(line(6).contains("model_id=\"api:openrouter:qwen/qwen3-coder\""));
        assert!(line(7).contains("model=\"Codex (vendor agent)\""));
        assert!(line(8).contains("target=\"cli:codex\""));
    }

    #[test]
    fn cache_returns_exact_prepared_bytes_and_refuses_unknown_ids() {
        let cache = DiagnosticExports::default();
        let report = json!({"checks":[{"id":"runtime","status":"pass"}]});
        let prepared = cache.prepare(&report, &Value::Null).unwrap();
        let id = prepared["id"].as_str().unwrap();
        assert_eq!(cache.get(id).unwrap()["content"], prepared["content"]);
        assert!(cache.get("00000000000000000000000000000000").is_err());
        for _ in 0..4 {
            cache.prepare(&report, &Value::Null).unwrap();
        }
        assert!(cache.get(id).is_err());
        let latest = cache.prepare(&report, &Value::Null).unwrap();
        let latest_id = latest["id"].as_str().unwrap();
        {
            let mut snapshots = cache.0.lock().unwrap();
            snapshots.back_mut().unwrap().created = Instant::now() - TTL;
        }
        assert!(cache.get(latest_id).is_err());
    }

    #[test]
    fn projection_bounds_check_count_and_refuses_unknown_status() {
        let mut checks = vec![json!({"id":"runtime","status":"fail"}); MAX_CHECKS + 1];
        checks[0]["status"] = json!("passed");
        let content = project(
            &json!({"checks":checks}),
            &Value::Null,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        let value: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["checks"].as_array().unwrap().len(), MAX_CHECKS - 1);
        assert_eq!(value["omitted_checks"], 2);
        assert!(!content.contains("passed"));
    }
}
