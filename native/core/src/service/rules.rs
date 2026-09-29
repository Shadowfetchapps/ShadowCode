//! `/api/rules…`: the user's rulebook (Settings › Rules & skills). Profile
//! rules, skills, commands and agents merged with the project's own files;
//! switches, the profile `AGENTS.md` editor, what each agent reads, the
//! skill checker, Git imports, the optional export and starter skills.
use super::*;
use crate::rulebook::{self, check, delivery, export, import, starters};

#[derive(Default, Deserialize)]
#[serde(default)]
struct RulesBody {
    workspace: Text,
    id: Text,
    enabled: Flag,
    content: Text,
    expected_hash: Text,
    url: Text,
    names: Loose<Vec<String>>,
}

impl Service {
    fn rules_workspace(&self) -> Option<Workspace> {
        self.workspace().ok().and_then(|p| Workspace::open(&p).ok())
    }
    /// A request that names a project must name the selected one, so a
    /// stale screen cannot switch files of another project.
    fn same_project(&self, body: &RulesBody) -> Result<Option<Workspace>> {
        let workspace = self.rules_workspace();
        if let Some(named) = body.workspace.non_empty() {
            ensure!(
                workspace
                    .as_ref()
                    .is_some_and(|w| w.path.to_string_lossy() == named),
                "Project changed; refresh Rules & skills first"
            );
        }
        Ok(workspace)
    }
    pub(super) async fn rules_routes(&self, call: &Arc<Call>) -> Result<Value> {
        let paths = self.engine.paths().clone();
        let body: RulesBody = call.body()?;
        match (call.method.as_str(), &call.parts()[1..]) {
            ("GET", ["rules"]) => {
                let workspace = self.rules_workspace();
                let mut value = {
                    let paths = paths.clone();
                    let path = workspace.as_ref().map(|w| w.path.clone());
                    tokio::task::spawn_blocking(move || {
                        let workspace = path.and_then(|p| Workspace::open(&p).ok());
                        rulebook::overview(&paths, workspace.as_ref())
                    })
                    .await?
                };
                value["imports"] = json!(import::list(&paths).await);
                value["starters"] = starters::list(&paths);
                Ok(value)
            }
            ("PUT", ["rules", "profile"]) => {
                let content = body.content.as_str().to_owned();
                let expected = body.expected_hash.as_str().to_owned();
                let hash = tokio::task::spawn_blocking(move || {
                    rulebook::save_rules(&paths, &content, &expected)
                })
                .await??;
                Ok(json!({"ok": true, "hash": hash}))
            }
            ("POST", ["rules", "items"]) => {
                let workspace = self.same_project(&body)?;
                rulebook::set_enabled(
                    &paths,
                    workspace.as_ref().map(|w| w.path.as_path()),
                    body.id.as_str(),
                    body.enabled.is_true(),
                )?;
                Ok(json!({"ok": true, "id": body.id.as_str(), "enabled": body.enabled.is_true()}))
            }
            ("POST", ["rules", "sharing"]) => {
                let enabled = body.enabled.is_true();
                rulebook::State::update(&paths, |state| {
                    state.share_with_cli_agents = enabled;
                    Ok(())
                })?;
                Ok(json!({"ok": true, "share_with_cli_agents": enabled}))
            }
            ("GET", ["rules", "preview"]) => {
                let workspace = self
                    .rules_workspace()
                    .context("Open a project to see what each agent reads")?;
                let path = workspace.path.clone();
                tokio::task::spawn_blocking(move || -> Result<Value> {
                    let workspace = Workspace::open(&path)?;
                    let book = rulebook::Book::load(&paths, Some(&workspace.path));
                    Ok(delivery::preview(&book, &workspace))
                })
                .await?
            }
            ("GET", ["rules", "check"]) => {
                let path = self.rules_workspace().map(|w| w.path);
                tokio::task::spawn_blocking(move || {
                    let workspace = path.and_then(|p| Workspace::open(&p).ok());
                    let book =
                        rulebook::Book::load(&paths, workspace.as_ref().map(|w| w.path.as_path()));
                    check::run(&book, workspace.as_ref())
                })
                .await
                .map_err(Into::into)
            }
            ("POST", ["rules", "imports"]) => import::add(&paths, body.url.as_str()).await,
            ("POST", ["rules", "imports", name, "update"]) => import::update(&paths, name).await,
            ("DELETE", ["rules", "imports", name]) => {
                let name = name.to_string();
                tokio::task::spawn_blocking(move || import::remove(&paths, &name)).await??;
                Ok(json!({"ok": true}))
            }
            ("GET", ["rules", "export"]) => export::status(&paths),
            ("POST", ["rules", "export", target]) => export::enable(&paths, target),
            ("DELETE", ["rules", "export", target]) => export::disable(&paths, target),
            ("GET", ["rules", "starters"]) => Ok(json!({"starters": starters::list(&paths)})),
            ("POST", ["rules", "starters"]) => {
                starters::install(&paths, body.names.0.as_deref().unwrap_or_default())
            }
            // The desktop opens this folder in the file manager.
            ("POST", ["rules", "folder"]) => {
                let dir = rulebook::ensure_profile(&paths)?;
                Ok(json!({"path": dir}))
            }
            _ => Err(call.unavailable()),
        }
    }
}
