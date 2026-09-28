//! The composer's @-mention search, `GET /api/workspace/mentions?q=&limit=`
//! (routed from `workspace.rs`, whose family it belongs to): files and
//! folders of the selected project, best fuzzy matches first, skipping what
//! `.gitignore` ignores. See `crate::mentions`.
use super::*;

impl Service {
    pub(super) fn mention_search(&self, call: &Call) -> Result<Value> {
        let workspace = Workspace::open(&self.workspace()?)?;
        crate::mentions::search(&workspace.path, call.q("q"), call.limit(30, 100))
    }

    pub(super) fn context_preview(&self, call: &Call) -> Result<Value> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Body {
            mentions: Vec<crate::mentions::Mention>,
        }

        let body: Body = call.body()?;
        let workspace = Workspace::open(&self.workspace()?)?;
        let mentions = crate::mentions::validate(&workspace, body.mentions)?;
        Ok(serde_json::to_value(crate::mentions::preview(
            &workspace, &mentions,
        ))?)
    }
}
