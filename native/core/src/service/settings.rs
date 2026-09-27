//! `/api/config`, `/api/routing`, `/api/onboarding`, `/api/health`,
//! `/api/version`, `/api/doctor` and `/api/guardian…`: saved settings,
//! first-run setup and diagnostics. Doctor and the Guardian health check
//! await; the rest is synchronous and runs on the blocking pool.
use super::*;

#[derive(Default, Deserialize)]
#[serde(default)]
struct OnboardingBody {
    workspace: Text,
    permission_level: Text,
    permission_mode: Text,
    network: Flag,
    theme: Text,
    api_key: Text,
}

impl Service {
    pub(super) async fn settings_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/doctor") => self.doctor(call.q("test_model") == "true").await,
            ("GET", path) if path.starts_with("/api/diagnostic-exports/") => self
                .diagnostic_exports
                .get(path.trim_start_matches("/api/diagnostic-exports/")),
            ("POST", "/api/guardian/run") => {
                let cfg = crate::guardian::from_config_value(&self.config()?.guardian);
                let workspace = self.workspace()?;
                let guardian = self.guardian.clone();
                tokio::task::spawn_blocking(move || guardian.run_health_check(&cfg, &workspace))
                    .await?
            }
            _ => self.blocking(call, Self::settings_sync).await,
        }
    }
    fn settings_sync(&self, call: &Call) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/version") => Ok(
                json!({"name":"ShadowCode","version":crate::VERSION,"runtime":"rust","transport":"native","pid":std::process::id()}),
            ),
            ("GET", "/api/health") => {
                let cfg = self.config()?;
                let vendor = crate::cli_agent::Vendor::from_provider(&cfg.model.provider);
                let provider_detail = if cfg.model.provider == "mock" {
                    "Select a model to run coding tasks"
                } else if vendor.is_some() {
                    "Vendor CLI agent; login stays with the official CLI. Use Doctor for install/login status."
                } else {
                    "Configured; use Test model to verify connectivity"
                };
                Ok(
                    json!({"ok":true,"app":"ShadowCode","version":crate::VERSION,"workspace":self.workspace()?,"model":cfg.model,"onboarding":cfg.onboarding,"trusted":cfg.is_trusted(&self.workspace()?),"permissions":cfg.permissions,"provider":{"ok":cfg.model.provider!="mock","name":cfg.model.provider,"detail":provider_detail},"runtime":"rust","cli_agents":cfg.cli_agents}),
                )
            }
            ("GET", "/api/config") => config_view(&self.config()?),
            ("PUT", "/api/config") => self.save_config(call),
            ("GET", "/api/routing") => {
                let cfg = self.config()?;
                self.register(&cfg.model)?;
                routing::view(&self.engine.store(), &cfg)
            }
            ("PUT", "/api/routing") => {
                let store = self.engine.store();
                let values = &call.body["values"];
                routing::validate(values)?;
                let cfg = self.config()?;
                // An explicit edit must name a known model. Missing models in
                // previously saved configurations are handled as visible fallbacks.
                for role in routing::PURPOSES {
                    if let Some(id) = values[role]
                        .as_str()
                        .filter(|id| !matches!(*id, "" | "default" | "mock"))
                    {
                        let model = model_registry::resolve(&store, id, &cfg.model)?;
                        ensure!(model.provider != "mock", "Choose a coding model for {role}");
                    }
                }
                let cfg = Config::patch(self.engine.paths(), json!({"routing":values}))?;
                routing::view(&store, &cfg)
            }
            ("GET", "/api/onboarding") => {
                // Onboarding asks only for a folder and a permission mode; the
                // model picker is the first model choice, so nothing here
                // probes local servers or vendor CLIs.
                let cfg = self.config()?;
                Ok(
                    json!({"completed":cfg.onboarding["completed"].as_bool().unwrap_or(false),"suggested_workspace":self.workspace()?,"levels":["read_only","workspace","elevated"],"defaults":{"permission_level":"workspace","permission_mode":"ask","theme":"system"}}),
                )
            }
            ("POST", "/api/onboarding") => self.onboard(call),
            ("GET", "/api/guardian") => self.guardian.status(
                &crate::guardian::from_config_value(&self.config()?.guardian),
                &self.workspace()?,
            ),
            ("POST", "/api/guardian/request-patch") => self.guardian.request_prepare_patch(
                &crate::guardian::from_config_value(&self.config()?.guardian),
                &self.workspace()?,
                call.text("summary"),
            ),
            ("POST", "/api/guardian/approve-patch") => self.guardian.approve_prepare_patch(
                &crate::guardian::from_config_value(&self.config()?.guardian),
                &self.workspace()?,
                &self.engine.paths().data.join("guardian-proposals"),
                call.text("approval_id"),
            ),
            _ => Err(call.unavailable()),
        }
    }
    /// PUT /api/config: merge `values` into the saved configuration. The API
    /// key is never saved in the file; it goes to the secrets store.
    fn save_config(&self, call: &Call) -> Result<Value> {
        let text = |key: &str| call.text(key);
        let mut values = call.body["values"].clone();
        ensure!(values.is_object(), "values must be an object");
        if let Some(object) = values.as_object_mut() {
            object.remove("api_key");
        }
        // Choosing a user-facing mode means shell asks again, unless
        // the same request sets approve_shell explicitly.
        if values.pointer("/permissions/mode").is_some()
            && values.pointer("/permissions/approve_shell").is_none()
        {
            values["permissions"]["approve_shell"] = json!(true);
        }
        let cfg = Config::update(self.engine.paths(), |cfg| {
            // Settings use the provider's model name as `default`. Give
            // that configuration a stable identity before saving it.
            if values["model"].is_object() {
                let old = cfg.model.clone();
                let mut merged = serde_json::to_value(&old)?;
                config::merge(&mut merged, values["model"].clone());
                let mut model: ModelConfig = serde_json::from_value(merged)?;
                if model.default == model.name || !model_registry::same_target(&old, &model) {
                    model.default =
                        model_registry::model_id(&model.provider, &model.endpoint, &model.name);
                }
                model_registry::validate(&model)?;
                self.check_model_identity(&model)?;
                self.register(&old)?;
                values["model"] = json!(model);
            }
            let mut preview = serde_json::to_value(&*cfg)?;
            config::merge(&mut preview, values.clone());
            let updated: Config = serde_json::from_value(preview)?;
            updated.validate()?;
            if !text("api_key").is_empty() {
                let key = if text("api_key_env").is_empty() {
                    values
                        .pointer("/model/api_key_env")
                        .and_then(Value::as_str)
                        .unwrap_or(&cfg.model.api_key_env)
                } else {
                    text("api_key_env")
                };
                config::set_secret(self.engine.paths(), key, text("api_key"))?;
            }
            *cfg = updated;
            Ok(())
        })?;
        // A successful settings save is authoritative immediately, even if
        // no follow-up catalog request arrives before an old probe finishes.
        self.engine
            .vendors()
            .configure(&cfg.cli_agents, cfg.offline());
        self.register(&cfg.model)?;
        config_view(&cfg)
    }
    /// POST /api/onboarding: save the first-run choices, trust the chosen
    /// folder and open a conversation in it.
    fn onboard(&self, call: &Call) -> Result<Value> {
        let body: OnboardingBody = call.body()?;
        let workspace = expand_path(body.workspace.as_str())?;
        let workspace = Workspace::open(&workspace)?.path;
        let cfg = Config::update(self.engine.paths(), |cfg| {
            cfg.model = self.model_from_body(&call.body, &cfg.model);
            cfg.permissions.level =
                serde_json::from_value(json!(if body.permission_level.is_empty() {
                    "workspace"
                } else {
                    body.permission_level.as_str()
                }))?;
            cfg.permissions.network = body.network.is_true();
            // New installs start restricted: every file edit and
            // command asks unless the user picked "Allow project edits".
            cfg.permissions.mode = if body.permission_mode.as_str() == "allow_edits" {
                crate::config::PermissionMode::AllowEdits
            } else {
                crate::config::PermissionMode::Ask
            };
            cfg.permissions.approve_shell = true;
            cfg.ui["theme"] = json!(if body.theme.is_empty() {
                "light"
            } else {
                body.theme.as_str()
            });
            cfg.onboarding = json!({"completed":true,"workspace":workspace});
            cfg.grant_trust(&workspace);
            cfg.validate()?;
            if !body.api_key.is_empty() {
                config::set_secret(
                    self.engine.paths(),
                    &cfg.model.api_key_env,
                    body.api_key.as_str(),
                )?;
            }
            Ok(())
        })?;
        let session = self
            .engine
            .store()
            .create_session(&workspace, &cfg.model.default, "")?;
        let sid = session["id"]
            .as_str()
            .context("Session missing ID")?
            .to_owned();
        self.select(&workspace, Some(sid.clone()))?;
        self.register(&cfg.model)?;
        Ok(json!({"ok":true,"workspace":workspace,"session_id":sid}))
    }
}

/// GET/PUT /api/config response: the saved configuration plus what the
/// permission modes mean for each vendor runtime.
pub(super) fn config_view(cfg: &Config) -> Result<Value> {
    let mut value = json!(cfg);
    value["permissions"]["vendor_notes"] = permissions::vendor_notes(cfg);
    value["network"]["offline"] = json!(cfg.offline());
    Ok(value)
}
