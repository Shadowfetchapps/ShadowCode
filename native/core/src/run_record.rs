//! The run record: what exactly ran a job, kept with the job (`Job::run`),
//! shown under "Run details" on the finished task and included in the
//! diagnostic export. It holds identifiers and short hashes only, never
//! prompts, file contents, paths or keys.
use crate::config::Config;
use serde::{Deserialize, Serialize};

/// Length of the short hashes shown to people.
const SHORT: usize = 12;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunRecord {
    /// The exact model id (picker id, e.g. `api:openrouter:qwen/qwen3-coder`
    /// or `cli:codex`).
    pub model_id: String,
    /// The model name sent to the provider.
    pub model: String,
    pub provider: String,
    /// `vendor_cli`, `local_llamacpp` or `native_http`.
    pub route: String,
    /// The vendor CLI that ran the job ("Codex") and its `--version`.
    pub vendor: Option<String>,
    pub vendor_version: Option<String>,
    /// `low`, `medium` or `high`; `None` is the model's default.
    pub effort: Option<String>,
    pub app_version: String,
    /// The commit ShadowCode was built from, when the build recorded it.
    pub app_commit: Option<String>,
    /// Short SHA-256 of the effective settings the job ran with.
    pub settings_hash: String,
    /// Short SHA-256 of the rules and skills text delivered to the agent;
    /// `None` when none was delivered.
    pub rules_hash: Option<String>,
    pub recorded_at: f64,
}

/// A short SHA-256 of `bytes`.
pub fn short_hash(bytes: &[u8]) -> String {
    let mut hash = crate::workspace::hash(bytes);
    hash.truncate(SHORT);
    hash
}

/// Short hash of the settings, with the per-task runtime fields as the job
/// saw them. Secrets are never part of the settings file.
pub fn settings_hash(config: &Config) -> String {
    short_hash(&serde_json::to_vec(config).unwrap_or_default())
}

/// The rules hash of a delivered text (`None` for none).
pub fn rules_hash(text: &str) -> Option<String> {
    (!text.trim().is_empty()).then(|| short_hash(text.as_bytes()))
}

impl RunRecord {
    /// The record for a job about to run with `config`.
    pub fn new(config: &Config, effort: Option<&str>) -> Self {
        let (_, route) = crate::routing::route_of(&config.model);
        Self {
            model_id: config.model.default.clone(),
            model: config.model.name.clone(),
            provider: config.model.provider.clone(),
            route: route.to_owned(),
            effort: effort.map(str::to_owned),
            app_version: crate::VERSION.to_owned(),
            app_commit: crate::updates::commit().map(str::to_owned),
            settings_hash: settings_hash(config),
            recorded_at: crate::now(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_identifiers_and_short_stable_hashes() {
        let mut config = Config::default();
        config.model.default = "api:openrouter:acme/coder".into();
        config.model.name = "acme/coder".into();
        config.model.provider = "openrouter".into();
        let record = RunRecord::new(&config, Some("high"));
        assert_eq!(record.model_id, "api:openrouter:acme/coder");
        assert_eq!(record.provider, "openrouter");
        assert_eq!(record.route, "native_http");
        assert_eq!(record.effort.as_deref(), Some("high"));
        assert_eq!(record.app_version, crate::VERSION);
        assert_eq!(record.settings_hash.len(), 12);
        assert_eq!(record.settings_hash, settings_hash(&config), "stable");
        config.agent.max_steps += 1;
        assert_ne!(record.settings_hash, settings_hash(&config));
        assert_eq!(rules_hash("  "), None);
        assert_eq!(rules_hash("Use tabs.").unwrap().len(), 12);
        assert_ne!(rules_hash("Use tabs."), rules_hash("Use spaces."));
        // Never anything but ids and hashes.
        let text = serde_json::to_string(&record).unwrap();
        assert!(!text.contains("trusted_workspaces"));
    }
}
