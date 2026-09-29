//! `/api/agents`, `/api/subagents…` and `/api/roles` routes. Definitions
//! live in `crate::agents`; runs in `crate::subagents`; a project's roles in
//! `crate::roles`.
use super::*;
use crate::{
    agents,
    roles::{self, Role},
    store::keys,
    subagents,
};

/// POST /api/roles: change a project's roles. Absent fields keep their value.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RolesBody {
    workspace: Text,
    session_id: Text,
    /// Code tasks run as Plan → Implement → Review.
    pipeline: Flag,
    /// A preset id (`crate::roles::PRESETS`), applied before the roles below.
    preset: Text,
    /// `""` (the conversation's model), `skip` (plan, review) or a picker id.
    plan: Option<Value>,
    implement: Option<Value>,
    review: Option<Value>,
    explore: Option<Value>,
}

impl Service {
    /// The project whose roles apply, and the conversation, for a request.
    fn roles_scope(&self, workspace: &str, session: &str) -> Result<(PathBuf, Option<String>)> {
        let workspace = if workspace.is_empty() {
            Workspace::open(&self.workspace()?)?.path
        } else {
            Workspace::open(&expand_path(workspace)?)?.path
        };
        let session = match session {
            "" => self
                .current_session()?
                .filter(|_| self.workspace().is_ok_and(|w| w == workspace)),
            id => Some(id.to_owned()),
        };
        let store = self.engine.store();
        Ok((
            roles::project_of(&store, &workspace, session.as_deref()),
            session,
        ))
    }
    /// The model a conversation's next turn runs on (its remembered picker
    /// id, else the project's, else the configured default).
    fn conversation_model(
        &self,
        config: &Config,
        project: &Path,
        session: Option<&str>,
    ) -> Result<crate::config::ModelConfig> {
        let store = self.engine.store();
        let target = match session {
            Some(sid) => store.session_meta(sid, keys::EXECUTION_TARGET)?,
            None => None,
        }
        .or(store.native_meta(&keys::execution_target(project))?);
        Ok(target
            .and_then(|id| self.resolve_model(&id, &config.model).ok())
            .unwrap_or_else(|| config.model.clone()))
    }
    fn roles_view(&self, project: &Path, session: Option<&str>) -> Result<Value> {
        let config = Config::load(self.engine.paths(), Some(project))?;
        let conversation = self.conversation_model(&config, project, session)?;
        roles::view(
            &self.engine.store(),
            &config,
            project,
            session,
            &conversation,
        )
    }
    fn save_roles(&self, call: &Call) -> Result<Value> {
        let body: RolesBody = call.body()?;
        let (project, session) =
            self.roles_scope(body.workspace.as_str(), body.session_id.as_str())?;
        let config = Config::load(self.engine.paths(), Some(&project))?;
        ensure!(
            config.is_trusted(&project),
            "Trust this project before choosing its roles"
        );
        let store = self.engine.store();
        let mut setup = roles::load(&store, &project)?;
        if let Some(id) = body.preset.non_empty() {
            let preset =
                roles::preset(id).with_context(|| format!("Unknown roles preset '{id}'"))?;
            let local = if preset.roles.contains(&roles::LOCAL) {
                let conversation =
                    self.conversation_model(&config, &project, session.as_deref())?;
                self.engine
                    .local_choice(
                        &config,
                        &project,
                        Some(conversation.default.as_str())
                            .filter(|id| id.starts_with("local:gguf:")),
                    )?
                    .map(|(id, _)| id)
            } else {
                None
            };
            roles::apply_preset(&mut setup, preset, local.as_deref())?;
        }
        for (role, value) in [
            (Role::Plan, &body.plan),
            (Role::Implement, &body.implement),
            (Role::Review, &body.review),
            (Role::Explore, &body.explore),
        ] {
            let Some(value) = value else { continue };
            let value = match value {
                Value::Null => "",
                Value::String(text) => text.trim(),
                _ => bail!("The {} role must be a model id", role.id()),
            };
            if setup.get(role) != value {
                setup.set(role, value);
                setup.preset.clear();
            }
        }
        if let Some(pipeline) = body.pipeline.0 {
            setup.pipeline = pipeline;
        }
        setup.validate()?;
        // A model that cannot be resolved is refused now, not when a task
        // reaches that role.
        for role in Role::ALL {
            let id = setup.get(role);
            if !matches!(id, "" | roles::SKIP) {
                let model = self.resolve_model(id, &config.model).with_context(|| {
                    format!("The {} role's model ({id}) is not available", role.id())
                })?;
                ensure!(
                    model.provider != "mock",
                    "Choose a local or compatible model for the {} role",
                    role.id()
                );
            }
        }
        roles::save(&store, &project, &setup)?;
        self.roles_view(&project, session.as_deref())
    }

    pub(super) fn agent_routes(&self, call: &Call) -> Result<Value> {
        let store = self.engine.store();
        match (call.method.as_str(), &call.parts()[1..]) {
            // Agent definitions for this project (project, user, built-in).
            ("GET", ["agents"]) => {
                let workspace = if call.q("workspace").is_empty() {
                    Workspace::open(&self.workspace()?)?
                } else {
                    Workspace::open(&expand_path(call.q("workspace"))?)?
                };
                let user = agents::user_dir(self.engine.paths());
                let mut value = agents::discover(&workspace, Some(&user)).to_json();
                let config = Config::load(self.engine.paths(), Some(&workspace.path))?;
                value["settings"] = json!(subagents::SubagentsConfig::from_config(&config));
                value["user_dir"] = json!(user);
                value["workspace"] = json!(workspace.path);
                Ok(value)
            }
            // Runs started from one conversation, oldest first.
            ("GET", ["subagents"]) => {
                let session = call.q("session_id");
                ensure!(!session.is_empty(), "Give session_id");
                Ok(json!({"runs": subagents::list(&store, session)?}))
            }
            ("GET", ["subagents", id]) => Ok(json!(subagents::get(&store, id)?)),
            // A project's roles, resolved for a conversation.
            ("GET", ["roles"]) => {
                let (project, session) =
                    self.roles_scope(call.q("workspace"), call.q("session_id"))?;
                self.roles_view(&project, session.as_deref())
            }
            ("POST", ["roles"]) => self.save_roles(call),
            _ => Err(call.unavailable()),
        }
    }
}
