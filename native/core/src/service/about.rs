//! `/api/about` and `/api/updates…`: Settings › About and the update notice
//! (see [`crate::updates`]). Only the check routes reach the network, and
//! only GitHub's releases API.
use super::*;
use crate::updates::{self, Trigger};

#[derive(Default, Deserialize)]
#[serde(default)]
struct DismissBody {
    version: Text,
}

impl Service {
    pub(super) async fn about_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/about") => {
                self.blocking(call, |service, _| {
                    Ok(updates::about(service.engine.paths(), &service.config()?))
                })
                .await
            }
            // `?auto=1`: the window's daily check, which runs only when it
            // is due, allowed and online; otherwise this reads the cache.
            ("GET", "/api/updates") => {
                if call.q("auto") == "1" {
                    let config = self.config()?;
                    updates::run_check(self.engine.paths(), &config, Trigger::Automatic).await?;
                }
                self.update_status()
            }
            ("POST", "/api/updates/check") => {
                let config = self.config()?;
                updates::run_check(self.engine.paths(), &config, Trigger::Manual).await?;
                self.update_status()
            }
            ("POST", "/api/updates/dismiss") => {
                let body: DismissBody = call.body()?;
                updates::dismiss(self.engine.paths(), body.version.as_str())?;
                self.update_status()
            }
            _ => Err(call.unavailable()),
        }
    }
    fn update_status(&self) -> Result<Value> {
        Ok(updates::status(
            self.engine.paths(),
            &self.config()?,
            &updates::policy(),
            updates::install_kind(),
        ))
    }
}
