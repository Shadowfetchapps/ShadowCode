//! `/api/local-models/downloads…`: the built-in catalog of free models this
//! computer can download, with a recommendation from its memory and graphics
//! card. Downloads start only from these routes (a click), never on their
//! own, and are refused in offline mode.
use super::*;
use crate::local_downloads::{self, CatalogModel};

#[derive(Default, Deserialize)]
#[serde(default)]
struct DownloadBody {
    id: Text,
}

const OFFLINE_STOPPED: &str = "Offline mode was turned on, so the download stopped. Switch back to Online, then choose Resume to continue.";
pub(super) const OFFLINE: &str = "ShadowCode is in offline mode. Switch the network mode to Online in Settings › Permissions & network to download a model.";

impl Service {
    pub(super) async fn local_download_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let cfg = self.config()?;
        let dir = local_downloads::dir(self.engine.paths());
        let model = || -> Result<&'static CatalogModel> {
            let body: DownloadBody = call.body()?;
            local_downloads::entry(body.id.as_str())
                .context("That model is not in ShadowCode's download list")
        };
        let downloads = local_downloads::downloader();
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/local-models/downloads") => {}
            ("POST", "/api/local-models/downloads/start") => {
                let model = model()?;
                ensure!(!cfg.offline(), OFFLINE);
                let runtime = crate::local_engine::runtime(&cfg.local_engine.llama_binary);
                if let Some(list) = runtime.probe.as_ref().and_then(|p| p.architectures.clone()) {
                    ensure!(
                        list.contains(model.architecture),
                        "The bundled llama.cpp can't run {} ({} architecture), so it was not downloaded",
                        model.name,
                        model.architecture
                    );
                }
                // Turning offline mode on stops the download (Resume later).
                let paths = self.engine.paths().clone();
                let guard: local_downloads::Guard =
                    Arc::new(move || match Config::load(&paths, None) {
                        Ok(config) if config.offline() => Err(OFFLINE_STOPPED.into()),
                        _ => Ok(()),
                    });
                downloads.start_guarded(
                    model.spec(),
                    dir.clone(),
                    local_downloads::client()?,
                    guard,
                )?;
            }
            ("POST", "/api/local-models/downloads/pause") => {
                downloads.pause(&dir, model()?.file);
            }
            ("POST", "/api/local-models/downloads/cancel") => {
                let file = model()?.file;
                let dir = dir.clone();
                tokio::task::spawn_blocking(move || downloads.cancel(&dir, file))
                    .await
                    .context("Download worker stopped")??;
            }
            ("POST", "/api/local-models/downloads/delete") => {
                let model = model()?;
                let id =
                    crate::local_engine::entry_id(&local_downloads::final_path(&dir, model.file));
                let local = self.engine.local_runtime();
                if local.loaded().is_some_and(|loaded| loaded.id == id) {
                    // Refused while a task uses it ("Stop the task first").
                    local.unload().await?;
                }
                let dir = dir.clone();
                tokio::task::spawn_blocking(move || downloads.delete(&dir, model.file))
                    .await
                    .context("Download worker stopped")??;
            }
            _ => return Err(call.unavailable()),
        }
        let offline = cfg.offline();
        let binary = cfg.local_engine.llama_binary.clone();
        tokio::task::spawn_blocking(move || {
            let runtime = crate::local_engine::runtime(&binary);
            let gpu = runtime.gpu().map(|g| (g.name.as_str(), g.total_bytes));
            let architectures = runtime.probe.as_ref().and_then(|p| p.architectures.clone());
            local_downloads::catalog_json(
                downloads,
                &dir,
                crate::local_engine::read_meminfo(),
                gpu,
                architectures.as_deref(),
                offline,
            )
        })
        .await
        .context("Download worker stopped")
    }
}
