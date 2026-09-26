//! `/api/compare…` routes. The engine side lives in `crate::compare`.
use super::*;
use crate::compare;

impl Service {
    pub(super) async fn compare(&self, call: &Call) -> Result<Value> {
        let (method, parts, body) = (call.method.as_str(), call.parts(), &call.body);
        let engine = &self.engine;
        let workspace_in = |value: &str| -> Result<PathBuf> {
            Ok(if value.is_empty() {
                self.workspace()?
            } else {
                Workspace::open(&expand_path(value)?)?.path
            })
        };
        let q = |key: &str| call.q(key);
        match (method, &parts[1..]) {
            ("POST", ["compare"]) => {
                let models = body["models"]
                    .as_array()
                    .context("models must be a list of 2 or 3 model ids")?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .context("models must be a list of model ids")
                    })
                    .collect::<Result<Vec<_>>>()?;
                let record = compare::start(
                    engine,
                    compare::StartOptions {
                        workspace: workspace_in(body["workspace"].as_str().unwrap_or(""))?,
                        task: body["task"].as_str().unwrap_or("").into(),
                        models,
                        mode: body["mode"].as_str().unwrap_or("code").into(),
                        web: body["web"].as_bool().unwrap_or(false),
                        owner: self.job_owner.as_ref(),
                    },
                )
                .await?;
                Ok(record.to_json())
            }
            ("GET", ["compares"]) => {
                let records = compare::list(engine, &workspace_in(q("workspace"))?).await?;
                Ok(json!({
                    "compares": records.iter().map(compare::Record::to_json).collect::<Vec<_>>()
                }))
            }
            ("GET", ["compare", "scoreboard"]) => {
                compare::scoreboard(engine, &workspace_in(q("workspace"))?).await
            }
            ("GET", ["compare", id]) => Ok(compare::get(engine, id).await?.to_json()),
            ("POST", ["compare", id, "keep"]) => {
                let model = body["model"]
                    .as_str()
                    .filter(|m| !m.is_empty())
                    .context("Choose the model whose result to keep")?;
                Ok(compare::keep_reviewed(
                    engine,
                    id,
                    model,
                    body["accept_unverified"].as_bool().unwrap_or(false),
                )
                .await?
                .to_json())
            }
            ("POST", ["compare", id, "recover"]) => {
                Ok(compare::recover(engine, id).await?.to_json())
            }
            ("POST", ["compare", id, "discard"]) => {
                Ok(compare::discard(engine, id).await?.to_json())
            }
            ("POST", ["compare", id, "cancel"]) => Ok(compare::cancel(engine, id).await?.to_json()),
            _ => bail!(
                "Application command is not available: {method} /{}",
                parts.join("/")
            ),
        }
    }
}
