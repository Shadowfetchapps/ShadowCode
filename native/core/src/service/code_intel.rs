//! `/api/code-intel/*`: language server status and managed installs, the
//! code index, search, the repo map, and optional embedding models.
use super::*;
use crate::code_intel::{embeddings, repo_map, search, CodeIntelConfig, ServerOverride};
use crate::lsp;
use std::collections::BTreeMap;

#[derive(Default, Deserialize)]
#[serde(default)]
struct PackageBody {
    package: Text,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ModelBody {
    model: Text,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SearchBody {
    query: Text,
    path: Text,
    max_hits: Loose<usize>,
}

/// Settings to change; fields left out keep their value. Unlike most
/// bodies this one is strict: a mistyped setting is an error, not ignored.
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SettingsBody {
    lsp: Option<bool>,
    diagnostics_on_edit: Option<bool>,
    diagnostics_wait_ms: Option<u64>,
    lsp_idle_minutes: Option<u64>,
    max_servers: Option<usize>,
    servers: Option<BTreeMap<String, ServerOverride>>,
    repo_map_tokens: Option<usize>,
    semantic_search: Option<bool>,
    embedding_model: Option<String>,
}

impl SettingsBody {
    fn apply(self, config: &mut CodeIntelConfig) {
        macro_rules! set {
            ($($field:ident),*) => {$(
                if let Some(value) = self.$field {
                    config.$field = value;
                }
            )*};
        }
        set!(
            lsp,
            diagnostics_on_edit,
            diagnostics_wait_ms,
            lsp_idle_minutes,
            max_servers,
            servers,
            repo_map_tokens,
            semantic_search,
            embedding_model
        );
    }
}

fn save_intel(
    paths: &AppPaths,
    edit: impl FnOnce(&mut CodeIntelConfig),
) -> Result<CodeIntelConfig> {
    let mut saved = None;
    Config::update(paths, |config| {
        let mut intel = CodeIntelConfig::lenient(config);
        edit(&mut intel);
        intel.validate()?;
        config
            .extra
            .insert("code_intel".into(), serde_json::to_value(&intel)?);
        saved = Some(intel);
        Ok(())
    })?;
    saved.context("Settings were not saved")
}

impl Service {
    pub(super) async fn code_intel_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let config = self.config()?;
        let data_dir = crate::code_intel::data_dir(self.engine.paths());
        let managed = lsp::managed_dir(&data_dir);
        let workspace = self.workspace()?;
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/code-intel/status") => {
                self.code_intel_status(&config, &data_dir, &workspace).await
            }
            ("POST", "/api/code-intel/config") => {
                let body: SettingsBody = serde_json::from_value(call.body.clone())
                    .map_err(|e| anyhow::anyhow!("Invalid code_intel settings: {e}"))?;
                let paths = self.engine.paths().clone();
                let next = tokio::task::spawn_blocking(move || {
                    save_intel(&paths, |intel| body.apply(intel))
                })
                .await
                .context("Settings worker stopped")??;
                if !next.lsp {
                    lsp::pool().stop_all(None).await;
                }
                Ok(json!({"ok": true, "config": next}))
            }
            ("POST", "/api/code-intel/install") => {
                ensure!(
                    !config.offline(),
                    "ShadowCode is in offline mode; switch the network mode to online to download a language server"
                );
                let body: PackageBody = call.body()?;
                let package = lsp::install::package(body.package.as_str())
                    .context("Choose typescript or python")?;
                let started = lsp::install::start(managed.clone(), package)?;
                Ok(
                    json!({"ok": true, "started": started, "managed": lsp::install::status(&managed)}),
                )
            }
            ("POST", "/api/code-intel/uninstall") => {
                let body: PackageBody = call.body()?;
                let package = lsp::install::package(body.package.as_str())
                    .context("Choose typescript or python")?;
                let removed = lsp::install::remove(&managed, package).await?;
                Ok(
                    json!({"ok": true, "removed": removed, "managed": lsp::install::status(&managed)}),
                )
            }
            ("POST", "/api/code-intel/servers/stop") => {
                let stopped = lsp::pool().stop_all(None).await;
                let embedding = embeddings::stop().await;
                Ok(json!({"ok": true, "stopped": stopped, "embedding_server_stopped": embedding}))
            }
            ("POST", "/api/code-intel/embeddings/install") => {
                ensure!(
                    !config.offline(),
                    "ShadowCode is in offline mode; switch the network mode to online to download a model"
                );
                let body: ModelBody = call.body()?;
                let entry = embeddings::catalog_entry(body.model.as_str())
                    .context("Unknown embedding model")?;
                let paths = self.engine.paths().clone();
                let (dir, root) = (data_dir.clone(), workspace.clone());
                let started = embeddings::start_install(data_dir.clone(), entry, move |result| {
                    if result.is_err() {
                        return;
                    }
                    // Use the new model unless another one was chosen, then
                    // embed the open project in the background.
                    let _ = save_intel(&paths, |intel| {
                        if intel.embedding_model.is_empty() {
                            intel.embedding_model = entry.id.into();
                        }
                    });
                    if let Ok(config) = Config::load(&paths, Some(&root)) {
                        if let Some(semantic) = embeddings::Semantic::resolve(&dir, &config) {
                            embeddings::spawn_backfill(root, semantic);
                        }
                    }
                });
                Ok(
                    json!({"ok": true, "started": started, "models": embeddings::catalog_json(&data_dir, &CodeIntelConfig::lenient(&config))}),
                )
            }
            ("POST", "/api/code-intel/embeddings/remove") => {
                let body: ModelBody = call.body()?;
                let entry = embeddings::catalog_entry(body.model.as_str())
                    .context("Unknown embedding model")?;
                embeddings::stop().await;
                let (paths, dir) = (self.engine.paths().clone(), data_dir.clone());
                let (removed, intel) = tokio::task::spawn_blocking(move || -> Result<_> {
                    let removed = embeddings::remove(&dir, entry)?;
                    let intel = save_intel(&paths, |intel| {
                        if intel.embedding_model == entry.id {
                            intel.embedding_model.clear();
                        }
                    })?;
                    Ok((removed, intel))
                })
                .await
                .context("Settings worker stopped")??;
                Ok(
                    json!({"ok": true, "removed": removed, "models": embeddings::catalog_json(&data_dir, &intel)}),
                )
            }
            ("POST", "/api/code-intel/index/focus") => {
                let focus = call.body["focus"].as_str().map(str::to_owned);
                let root = workspace.clone();
                tokio::task::spawn_blocking(move || {
                    crate::symbol_index::set_focus(&root, focus.as_deref())
                })
                .await
                .context("Index worker stopped")?
            }
            ("POST", "/api/code-intel/index/clear") => {
                let root = workspace.clone();
                tokio::task::spawn_blocking(move || {
                    crate::symbol_index::clear(&root)?;
                    crate::symbol_index::stats(&root)
                })
                .await
                .context("Index worker stopped")?
            }
            ("POST", "/api/code-intel/reindex") => {
                let root = workspace.clone();
                let stats = tokio::task::spawn_blocking(move || {
                    // Batches of changed files until every file the scan
                    // finds is indexed, for at most a minute and a half per
                    // request; the answer says whether more remains. A
                    // project over the file limit stops there too
                    // (`capped`): only a focus folder covers more of it.
                    let started = std::time::Instant::now();
                    loop {
                        let batch = crate::symbol_index::index_more(&root)?;
                        if batch["more"] != true
                            || started.elapsed() > std::time::Duration::from_secs(90)
                        {
                            break;
                        }
                    }
                    crate::symbol_index::stats(&root)
                })
                .await
                .context("Index worker stopped")??;
                let embedding = embeddings::Semantic::resolve(&data_dir, &config)
                    .map(|semantic| embeddings::spawn_backfill(workspace.clone(), semantic))
                    .unwrap_or(false);
                Ok(json!({"ok": true, "index": stats, "embedding_started": embedding}))
            }
            ("POST", "/api/code-intel/search") => {
                let body: SearchBody = call.body()?;
                let request = search::Request {
                    query: body.query.as_str().to_owned(),
                    path: body.path.as_str().to_owned(),
                    max_hits: body.max_hits.0.unwrap_or(10),
                };
                let semantic = embeddings::Semantic::resolve(&data_dir, &config);
                search::search_code(workspace, request, semantic).await
            }
            ("GET", "/api/code-intel/repo-map") => {
                let tokens = call.q("tokens").parse::<usize>().unwrap_or(1024);
                let task = call.q("query").to_owned();
                tokio::task::spawn_blocking(move || {
                    let focus = crate::symbol_index::recent_edits(&workspace, 12);
                    repo_map::build_json(&workspace, &focus, &task, tokens)
                })
                .await
                .context("Index worker stopped")?
            }
            _ => Err(call.unavailable()),
        }
    }

    async fn code_intel_status(
        &self,
        config: &Config,
        data_dir: &Path,
        workspace: &Path,
    ) -> Result<Value> {
        let (intel, config_error) = match CodeIntelConfig::from_config(config) {
            Ok(intel) => (intel, None),
            Err(error) => (CodeIntelConfig::default(), Some(format!("{error:#}"))),
        };
        let managed = lsp::managed_dir(data_dir);
        let (languages, managed_status, npm, node) = {
            let (intel, managed) = (intel.clone(), managed.clone());
            tokio::task::spawn_blocking(move || {
                (
                    lsp::languages(&intel, Some(&managed)),
                    lsp::install::status(&managed),
                    lsp::install::npm(),
                    lsp::servers::find_program("node"),
                )
            })
            .await
            .context("Status worker stopped")?
        };
        let root = workspace.to_owned();
        let index = tokio::task::spawn_blocking(move || crate::symbol_index::stats(&root))
            .await
            .context("Status worker stopped")?
            .unwrap_or(Value::Null);
        let active = embeddings::active(data_dir, &intel);
        let coverage = match active {
            Some(entry) => {
                let (root, dir) = (workspace.to_owned(), data_dir.to_owned());
                tokio::task::spawn_blocking(move || embeddings::coverage(&root, &dir, entry))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .map(|(embedded, chunks)| json!({"embedded": embedded, "chunks": chunks}))
            }
            None => None,
        };
        let runtime = crate::local_engine::runtime_candidates(&config.local_engine.llama_binary)
            .into_iter()
            .next()
            .map(|(path, _)| path);
        Ok(json!({
            "config": intel,
            "config_error": config_error,
            "offline": config.offline(),
            "languages": languages,
            "servers": lsp::pool().status(),
            "managed": managed_status,
            "managed_dir": managed,
            "npm": {"available": npm.is_some(), "path": npm, "node": node},
            "index": index,
            "embeddings": {
                "models": embeddings::catalog_json(data_dir, &intel),
                "active": active.map(|m| m.id),
                "runtime": runtime,
                "server": embeddings::running().await,
                "coverage": coverage,
                "backfill": embeddings::backfill_status(workspace),
            },
        }))
    }
}
