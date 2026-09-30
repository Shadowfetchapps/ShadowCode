//! `/api/data…`: Settings › Your data — backups, restore, repair and reset
//! (the rules live in `crate::data`). Refused over remote access: a backup
//! can hold API keys, and restore and reset replace everything.
use super::*;
use crate::data::{self, BackupOptions, RestoreOptions};

#[derive(Default, Deserialize)]
#[serde(default)]
struct DataBody {
    /// A backup folder (or its manifest.json, or a bare database copy).
    path: Text,
    /// Parent folder for a new backup; empty uses the default folder.
    folder: Text,
    include_secrets: Flag,
    /// Restore: remote access and paired devices too (restored switched off).
    include_remote: Flag,
    /// Reset asks for the word `reset` so a stray request cannot schedule it.
    confirm: Text,
}

fn absolute(path: &str, what: &str) -> Result<PathBuf> {
    ensure!(!path.trim().is_empty(), "Choose {what}");
    let path = expand_path(path.trim())?;
    ensure!(path.is_absolute(), "Give the full path of {what}");
    Ok(path)
}

impl Service {
    pub(super) async fn data_routes(&self, call: &Arc<Call>) -> Result<Value> {
        if (call.method.as_str(), call.path.as_str()) == ("POST", "/api/data/repair") {
            let paths = self.engine.paths().clone();
            let store = self.engine.store();
            let report = tokio::task::spawn_blocking(move || data::repair(&paths, &store))
                .await
                .context("Repair stopped")??;
            // In-memory caches: provider detection and vendor sign-in status
            // are probed again on the next look.
            *self.detection.lock().await = None;
            let vendors = self.engine.vendors();
            for vendor in crate::cli_agent::Vendor::ALL {
                vendors.clear(vendor).await;
            }
            return Ok(report);
        }
        self.blocking(call, Self::data_sync).await
    }

    /// Who holds the profile, and what to close so a scheduled restore or
    /// reset runs (a window attached to an editor's engine is not enough).
    fn engine_owner(&self) -> Value {
        let pid = std::process::id();
        json!({
            "mode": self.engine_mode(),
            "pid": pid,
            "restart": data::restart_hint(self.engine_mode(), pid),
        })
    }

    fn data_sync(&self, call: &Call) -> Result<Value> {
        let paths = self.engine.paths();
        let body: DataBody = call.body()?;
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/data") => {
                let mut overview = data::overview(paths)?;
                overview["engine"] = self.engine_owner();
                Ok(overview)
            }
            ("GET", "/api/data/backups") => {
                let folder = match call.q("folder") {
                    "" => data::backups_dir(paths),
                    folder => absolute(folder, "a backup folder")?,
                };
                Ok(json!({"folder": folder, "backups": data::list_backups(&folder)}))
            }
            ("POST", "/api/data/backups") => {
                let folder = body
                    .folder
                    .non_empty()
                    .map(|f| absolute(f, "a folder for the backup"))
                    .transpose()?;
                let (path, manifest) = data::create_backup(
                    paths,
                    BackupOptions {
                        include_secrets: body.include_secrets.is_true(),
                        folder: folder.as_deref(),
                        reason: "manual",
                        allow_raw: false,
                    },
                )?;
                Ok(json!({"path": path, "manifest": manifest}))
            }
            ("POST", "/api/data/backups/inspect") => {
                let path = absolute(body.path.as_str(), "a backup folder")?;
                Ok(json!(data::inspect(paths, &path)?))
            }
            ("POST", "/api/data/restore") => {
                let path = absolute(body.path.as_str(), "a backup folder")?;
                let pending = data::schedule_restore(
                    paths,
                    &path,
                    RestoreOptions {
                        include_secrets: body.include_secrets.is_true(),
                        include_remote: body.include_remote.is_true(),
                    },
                )?;
                Ok(json!({
                    "scheduled": true,
                    "pending": pending,
                    "message": format!(
                        "The restore finishes the next time ShadowCode starts. {}",
                        self.engine_owner()["restart"].as_str().unwrap_or("")
                    ),
                }))
            }
            ("POST", "/api/data/reset") => {
                ensure!(
                    body.confirm.as_str() == "reset",
                    "Send confirm: \"reset\" to schedule a reset"
                );
                let pending = data::schedule_reset(paths)?;
                Ok(json!({
                    "scheduled": true,
                    "pending": pending,
                    "message": format!(
                        "The reset happens the next time ShadowCode starts. {}",
                        self.engine_owner()["restart"].as_str().unwrap_or("")
                    ),
                }))
            }
            ("DELETE", "/api/data/pending") => {
                Ok(json!({"cancelled": data::cancel_pending(paths)?}))
            }
            _ => Err(call.unavailable()),
        }
    }
}
