//! `/api/data…`: Settings › Your data — backups, restore, repair and reset
//! (the rules live in `crate::data`). Refused over remote access: a backup
//! can hold API keys, and restore and reset replace everything.
use super::*;
use crate::data::{self, BackupOptions};

#[derive(Default, Deserialize)]
#[serde(default)]
struct DataBody {
    /// A backup folder (or its manifest.json, or a bare database copy).
    path: Text,
    /// Parent folder for a new backup; empty uses the default folder.
    folder: Text,
    include_secrets: Flag,
    /// Reset asks for the word `reset` so a stray request cannot schedule it.
    confirm: Text,
}

fn absolute(path: &str, what: &str) -> Result<PathBuf> {
    let path = expand_path(path.trim()).with_context(|| format!("Choose {what}"))?;
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

    fn data_sync(&self, call: &Call) -> Result<Value> {
        let paths = self.engine.paths();
        let body: DataBody = call.body()?;
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/data") => data::overview(paths),
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
                let pending = data::schedule_restore(paths, &path, body.include_secrets.is_true())?;
                Ok(json!({
                    "scheduled": true,
                    "pending": pending,
                    "message": "The restore finishes the next time ShadowCode starts. Quit ShadowCode (and any `shadowcode serve`) and open it again.",
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
                    "message": "The reset happens the next time ShadowCode starts. Quit ShadowCode (and any `shadowcode serve`) and open it again.",
                }))
            }
            ("DELETE", "/api/data/pending") => {
                Ok(json!({"cancelled": data::cancel_pending(paths)?}))
            }
            _ => Err(call.unavailable()),
        }
    }
}
