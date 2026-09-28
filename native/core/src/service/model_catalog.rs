//! `/api/providers…`, `/api/models…`, `/api/picker` and
//! `/api/local-models…`: the model registry, the composer picker, model
//! tests and the local GGUF catalog (downloads are in `local_downloads`),
//! plus the model helpers other route modules share (`model_from_body`,
//! `register`, `resolve_model`).
use super::*;

#[derive(Default, Deserialize)]
#[serde(default)]
struct LocalModelBody {
    id: Text,
    path: Text,
    root: Text,
    tag: Text,
}

impl Service {
    pub(super) async fn model_routes(&self, call: &Arc<Call>) -> Result<Value> {
        if call.path == "/api/local-models/downloads"
            || call.path.starts_with("/api/local-models/downloads/")
        {
            return self.local_download_routes(call).await;
        }
        let q = |key: &str| call.q(key);
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/providers/detect") => {
                Ok(json!({"providers":self.detected(q("refresh")=="1").await}))
            }
            ("GET", "/api/providers") => {
                let detected = self.detected(false).await;
                let mut presets = models::presets();
                for preset in &mut presets {
                    if let Some(found) = detected
                        .iter()
                        .find(|v| v["provider"] == preset["provider"])
                    {
                        preset["running"] = found["running"].clone();
                    }
                }
                Ok(json!({"providers":presets}))
            }
            ("GET", "/api/models") => {
                let store = self.engine.store();
                if q("detect") != "false" {
                    model_registry::record_detected(
                        &store,
                        &self.detected(q("refresh") == "1").await,
                    )?;
                }
                let cfg = self.config()?;
                self.register(&cfg.model)?;
                let mut models = model_registry::catalog(&store, &cfg.model)?;
                for vendor in crate::cli_agent::catalog_models(&cfg.cli_agents) {
                    if !models.iter().any(|row| {
                        row["id"] == vendor["id"] || row["provider"] == vendor["provider"]
                    }) {
                        models.push(vendor);
                    }
                }
                let picker = self.picker_catalog(&cfg, q("refresh") == "1").await?;
                Ok(json!({
                    "models":models,
                    "cli_agents":picker["vendors"],
                    "picker":picker["targets"],
                    "local_engine":picker["local_engine"]
                }))
            }
            ("GET", "/api/picker") => {
                let cfg = self.config()?;
                self.picker_catalog_with(&cfg, q("refresh") == "1", q("cached") == "1")
                    .await
            }
            ("GET", "/api/local-models") => {
                let cfg = self.config()?;
                let runtime = self.engine.clone();
                tokio::task::spawn_blocking(move || {
                    crate::local_engine::catalog_with(
                        &cfg.local_engine,
                        Some(runtime.local_runtime()),
                    )
                })
                .await
                .context("Local model catalog stopped")
            }
            ("POST", "/api/local-models/load") => {
                let store = self.engine.store();
                let cfg = self.config()?;
                let id = call.text("id");
                ensure!(
                    id.starts_with("local:gguf:"),
                    "Choose a local model to load"
                );
                crate::local_engine::entry_for_id(&cfg.local_engine, id)
                    .context("That local model is not in the catalog")?;
                let model = model_registry::resolve(&store, id, &cfg.model)?;
                // Unload aborts a load in progress; the lease ends right away
                // so the loaded model stays until another model is needed.
                // A model a running task holds is never waited for.
                let prepared = self
                    .engine
                    .prepare_model_client_now(&cfg, &model, &CancellationToken::new())
                    .await?;
                drop(prepared);
                Ok(json!({"ok":true,"loaded":self.engine.local_runtime().loaded_json()}))
            }
            ("POST", "/api/local-models/unload") => {
                let unloaded = self.engine.local_runtime().unload().await?;
                Ok(json!({"ok":true,"unloaded":unloaded}))
            }
            ("POST", "/api/models/test") => self.test_model(&call.body).await,
            _ => self.blocking(call, Self::model_routes_sync).await,
        }
    }
    fn model_routes_sync(&self, call: &Call) -> Result<Value> {
        let body: LocalModelBody = call.body()?;
        let local_catalog = |cfg: &Config| {
            crate::local_engine::catalog_with(&cfg.local_engine, Some(self.engine.local_runtime()))
        };
        match (call.method.as_str(), call.path.as_str()) {
            ("POST", "/api/local-models/add") => {
                let path = PathBuf::from(body.path.as_str());
                let cfg = self.config()?;
                let next = crate::local_engine::add(&cfg.local_engine, &path)?;
                Config::patch(self.engine.paths(), json!({"local_engine": next}))?;
                let cfg = self.config()?;
                Ok(json!({"ok":true,"local_engine":local_catalog(&cfg)}))
            }
            ("POST", "/api/local-models/remove") => {
                let key = if body.id.is_empty() {
                    body.path.as_str()
                } else {
                    body.id.as_str()
                };
                let cfg = self.config()?;
                let next = crate::local_engine::remove(&cfg.local_engine, key)?;
                Config::patch(self.engine.paths(), json!({"local_engine": next}))?;
                let cfg = self.config()?;
                Ok(
                    json!({"ok":true,"deleted_weights":false,"detail":"Catalog entry removed. Original weights were not deleted.","local_engine":local_catalog(&cfg)}),
                )
            }
            ("POST", "/api/local-models/import-ollama") => {
                let cfg = self.config()?;
                let root = if body.root.is_empty() {
                    crate::ollama_store::discover()
                        .context("No Ollama model store was found on this computer")?
                        .path
                } else {
                    let root = PathBuf::from(body.root.as_str());
                    ensure!(root.is_absolute(), "The Ollama store path must be absolute");
                    root
                };
                let next = crate::local_engine::import_ollama(
                    &cfg.local_engine,
                    &root,
                    body.tag.as_str(),
                )?;
                Config::patch(self.engine.paths(), json!({"local_engine": next}))?;
                let cfg = self.config()?;
                Ok(json!({"ok":true,"local_engine":local_catalog(&cfg)}))
            }
            ("POST", "/api/models/register" | "/api/models/select") => {
                let cfg = self.config()?;
                let model = if !call.text("provider").is_empty() {
                    self.model_from_body(&call.body, &cfg.model)
                } else {
                    self.resolve_model(body.id.as_str(), &cfg.model)?
                };
                self.check_model_identity(&model)?;
                self.register(&cfg.model)?;
                self.register(&model)?;
                if call.path.ends_with("select") {
                    Config::patch(self.engine.paths(), json!({"model":model}))?;
                }
                Ok(json!({"ok":true,"model":model}))
            }
            _ => Err(call.unavailable()),
        }
    }
    /// POST /api/models/test: one short completion (vendor CLIs report
    /// their install/login state instead), bounded to 45 seconds.
    async fn test_model(&self, body: &Value) -> Result<Value> {
        let text = |key: &str| body[key].as_str().unwrap_or("");
        let store = self.engine.store();
        let cfg = self.config()?;
        let model = if text("id").starts_with("local:gguf:") {
            crate::local_engine::entry_for_id(&cfg.local_engine, text("id"))
                .context("That local model is not in the catalog")?;
            model_registry::resolve(&store, text("id"), &cfg.model)?
        } else {
            self.model_from_body(body, &cfg.model)
        };
        ensure!(
            model.provider != "mock",
            "The offline preview is not a coding model"
        );
        if let Some(vendor) = crate::cli_agent::Vendor::from_provider(&model.provider) {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("/nonexistent"));
            let check =
                crate::cli_agent::doctor::check_vendor(vendor, &cfg.cli_agents, &home, None).await;
            let ready = check["state"] == "ready";
            return Ok(json!({
                "ok":ready,
                "reply":check["detail"],
                "error":if ready { Value::Null } else { check["fix"].clone() },
                "vendor":vendor.id(),
                "state":check["state"],
                "latency_ms":0
            }));
        }
        let started = Instant::now();
        let cancel = CancellationToken::new();
        // Local rows: refuses to swap a model a running task holds;
        // the lease in `prepared` lasts for this test.
        let prepared = self
            .engine
            .prepare_model_client_now(&cfg, &model, &cancel)
            .await?;
        let client: ModelClient = prepared.client(self.engine.paths())?;
        let model = prepared.config.clone();
        let result=tokio::time::timeout(Duration::from_secs(45),client.chat(&[json!({"role":"user","content":"Reply with one short sentence confirming you can respond."})],&[],cancel.clone(),|_|{})).await;
        Ok(match result {
            Ok(Ok(reply)) => {
                json!({"ok":!reply.text.trim().is_empty(),"reply":truncate(&reply.text,4000),"usage":reply.usage,"latency_ms":started.elapsed().as_millis(),"model":model.name,"capabilities":{"completion":true}})
            }
            Ok(Err(error)) => {
                json!({"ok":false,"error":format!("{error:#}"),"latency_ms":started.elapsed().as_millis()})
            }
            Err(_) => {
                cancel.cancel();
                json!({"ok":false,"error":"Model test timed out after 45 seconds","latency_ms":started.elapsed().as_millis()})
            }
        })
    }
    /// The composer picker: subscription rows from the vendor catalog and
    /// local GGUF rows from the local catalog, in one list.
    pub(super) async fn picker_catalog(&self, cfg: &Config, force: bool) -> Result<Value> {
        self.picker_catalog_with(cfg, force, false).await
    }
    async fn picker_catalog_with(&self, cfg: &Config, force: bool, cached: bool) -> Result<Value> {
        let vendors = self.engine.vendors();
        if !cached {
            vendors.refresh_all(&cfg.cli_agents, force).await;
        }
        let mut targets: Vec<Value> = Vec::new();
        let local_cfg = cfg.local_engine.clone();
        let engine = self.engine.clone();
        let local = tokio::task::spawn_blocking(move || {
            crate::local_engine::catalog_with(&local_cfg, Some(engine.local_runtime()))
        })
        .await
        .context("Local model catalog stopped")?;
        targets.extend(crate::local_engine::picker_rows(&local, &cfg.model.default));
        // Snapshot reads never fetch missing or stale OpenRouter metadata.
        // Full requests retain their existing first-fetch/background-refresh
        // behavior; the independent snapshot path makes local rows available
        // while that network request is pending.
        let paths = self.engine.paths();
        let offline = cfg.offline();
        let key_set = crate::openrouter::key(paths).is_some();
        let snapshot_only = cached;
        let catalog = match (key_set, crate::openrouter::cached(paths)) {
            (false, _) => None,
            (true, Some(cached)) => {
                if !snapshot_only && !offline && !crate::openrouter::is_fresh(&cached) {
                    let paths = paths.clone();
                    tokio::spawn(async move {
                        let _ = crate::openrouter::refresh(&paths, true).await;
                    });
                }
                Some(cached)
            }
            (true, None) if !snapshot_only && !offline => {
                crate::openrouter::refresh(paths, false).await.ok()
            }
            (true, None) => None,
        };
        targets.extend(crate::openrouter::picker_rows(
            catalog.as_ref(),
            key_set,
            offline,
        ));
        // Project last: provider observations may change while the local or
        // OpenRouter catalog is being read. Both vendor views must agree, and
        // an obsolete per-probe return must not overwrite a newer snapshot.
        let (rows, vendor_json) = vendors.picker_cached(&cfg.cli_agents).await;
        targets.splice(
            0..0,
            rows.iter()
                .map(crate::cli_agent::picker::PickerTarget::to_json),
        );
        // The composer shows its reasoning-effort control only on these.
        for row in &mut targets {
            row["reasoning"] = json!(crate::effort::row_supports(row));
        }
        Ok(json!({
            "targets": targets,
            "vendors": vendor_json,
            "local_engine": local,
            "generated_at": crate::now(),
        }))
    }
    pub(super) fn model_from_body(&self, body: &Value, fallback: &ModelConfig) -> ModelConfig {
        let provider = body["provider"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or(&fallback.provider);
        if let Some(vendor) = crate::cli_agent::Vendor::from_provider(provider).or_else(|| {
            crate::cli_agent::resolve_vendor(provider)
                .and_then(|m| crate::cli_agent::Vendor::from_provider(&m.provider))
        }) {
            let name = body["name"]
                .as_str()
                .filter(|v| !v.is_empty() && *v != vendor.provider() && *v != vendor.label())
                .or_else(|| body["model"].as_str().filter(|v| !v.is_empty()));
            return crate::cli_agent::vendor_model(vendor, name);
        }
        let preset = models::preset(provider);
        let provider_changed = provider != fallback.provider;
        let name = body["name"]
            .as_str()
            .filter(|v| !v.is_empty())
            .or_else(|| body["model"].as_str())
            .or_else(|| body["id"].as_str())
            .unwrap_or(&fallback.name);
        let endpoint = body["endpoint"]
            .as_str()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                if provider_changed {
                    preset["endpoint"].as_str().unwrap_or("")
                } else {
                    &fallback.endpoint
                }
            });
        let target_changed = provider_changed
            || endpoint.trim_end_matches('/') != fallback.endpoint.trim_end_matches('/');
        ModelConfig {
            default: if body["id"]
                .as_str()
                .is_some_and(|id| !id.is_empty() && id != name)
            {
                body["id"].as_str().unwrap().into()
            } else {
                model_registry::model_id(provider, endpoint, name)
            },
            provider: provider.into(),
            name: name.into(),
            endpoint: endpoint.into(),
            api_key_env: body["api_key_env"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| {
                    if target_changed {
                        preset["api_key_env"].as_str().unwrap_or("OPENAI_API_KEY")
                    } else {
                        &fallback.api_key_env
                    }
                })
                .into(),
            keep_alive: body["keep_alive"]
                .as_str()
                .unwrap_or(&fallback.keep_alive)
                .into(),
            context_limit: body["context_limit"]
                .as_u64()
                .map(|v| v as usize)
                .unwrap_or_else(|| {
                    if target_changed {
                        recommended_context(&json!(provider), None)
                    } else {
                        fallback.context_limit
                    }
                }),
        }
    }
    pub(super) fn register(&self, model: &ModelConfig) -> Result<()> {
        model_registry::validate(model)?;
        let store = self.engine.store();
        let mut metadata = store
            .models()?
            .into_iter()
            .find(|row| {
                row["id"] == model.default
                    && row["provider"] == model.provider
                    && row["endpoint"] == model.endpoint
            })
            .map(|row| row["metadata"].clone())
            .filter(Value::is_object)
            .unwrap_or(json!({}));
        metadata["api_key_env"] = json!(model.api_key_env);
        metadata["keep_alive"] = json!(model.keep_alive);
        store.upsert_model(&json!({"id":model.default,"name":model.name,"provider":model.provider,"endpoint":model.endpoint,"context_limit":model.context_limit,"metadata":metadata}))
    }
    pub(super) fn check_model_identity(&self, model: &ModelConfig) -> Result<()> {
        let current = self.config()?.model;
        if current.default == model.default {
            ensure!(model_registry::same_target(&current, model), "Model ID already belongs to a different provider, endpoint, or model; choose a unique ID");
        }
        if let Some(row) = self
            .engine
            .store()
            .models()?
            .iter()
            .find(|row| row["id"] == model.default)
        {
            ensure!(model_registry::same_target(&model_registry::from_row(row)?, model), "Model ID already belongs to a different provider, endpoint, or model; choose a unique ID");
        }
        Ok(())
    }
    pub(super) fn resolve_model(&self, id: &str, fallback: &ModelConfig) -> Result<ModelConfig> {
        model_registry::resolve(&self.engine.store(), id, fallback)
    }
}

fn recommended_context(provider: &Value, reported: Option<u64>) -> usize {
    model_registry::recommended_context(provider.as_str().unwrap_or(""), reported)
}
