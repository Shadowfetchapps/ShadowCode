//! `/api/logs…`: the app log for bug reports (`crate::applog`). The desktop
//! opens the folder in the file manager; nothing here returns log text.
use super::*;

impl Service {
    pub(super) async fn logs_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/logs") => self.blocking(call, Self::logs_status).await,
            // The desktop opens this folder in the file manager.
            ("POST", "/api/logs/folder") => {
                self.blocking(call, |service, _| {
                    let dir = crate::applog::dir(service.engine.paths());
                    crate::paths::private_directory(&dir)?;
                    Ok(json!({"path": dir}))
                })
                .await
            }
            _ => Err(call.unavailable()),
        }
    }

    /// GET /api/logs: where the log is and how big its files are.
    fn logs_status(&self, _call: &Call) -> Result<Value> {
        let paths = self.engine.paths();
        let files: Vec<Value> = crate::applog::files(paths)
            .into_iter()
            .map(|(name, bytes)| json!({"name": name, "bytes": bytes}))
            .collect();
        Ok(json!({
            "folder": crate::applog::dir(paths),
            "files": files,
            "max_file_bytes": crate::applog::MAX_BYTES,
            "max_files": crate::applog::FILES,
        }))
    }
}
