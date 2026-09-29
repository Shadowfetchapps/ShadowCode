//! `/api/agents` and `/api/subagents…` routes. Definitions live in
//! `crate::agents`; runs in `crate::subagents`.
use super::*;
use crate::{agents, subagents};

impl Service {
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
                let mut value = agents::discover_for(self.engine.paths(), &workspace).to_json();
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
            _ => Err(call.unavailable()),
        }
    }
}
