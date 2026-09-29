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

fn project(report: &Value, captured_at: &str) -> Result<String> {
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
        "excluded": ["credentials", "paths", "project names and source", "configured model names", "prompts and task events", "raw logs and crash reports"],
        "omitted_checks": omitted,
        "checks": selected,
    });
    let content = format!("{}\n", serde_json::to_string_pretty(&value)?);
    ensure!(
        content.len() <= MAX_BYTES,
        "Diagnostic export exceeds size limit"
    );
    Ok(content)
}

impl DiagnosticExports {
    pub(super) fn prepare(&self, report: &Value) -> Result<Value> {
        let captured_at = chrono::Utc::now().to_rfc3339();
        let content = project(report, &captured_at)?;
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
        let content = project(&report, "2026-01-01T00:00:00Z").unwrap();
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
    fn cache_returns_exact_prepared_bytes_and_refuses_unknown_ids() {
        let cache = DiagnosticExports::default();
        let report = json!({"checks":[{"id":"runtime","status":"pass"}]});
        let prepared = cache.prepare(&report).unwrap();
        let id = prepared["id"].as_str().unwrap();
        assert_eq!(cache.get(id).unwrap()["content"], prepared["content"]);
        assert!(cache.get("00000000000000000000000000000000").is_err());
        for _ in 0..4 {
            cache.prepare(&report).unwrap();
        }
        assert!(cache.get(id).is_err());
        let latest = cache.prepare(&report).unwrap();
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
        let content = project(&json!({"checks":checks}), "2026-01-01T00:00:00Z").unwrap();
        let value: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(value["checks"].as_array().unwrap().len(), MAX_CHECKS - 1);
        assert_eq!(value["omitted_checks"], 2);
        assert!(!content.contains("passed"));
    }
}
