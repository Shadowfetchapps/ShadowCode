//! `/api/secrets…`: where saved API keys live (`secrets.env` or the desktop
//! keyring) and moving them between the two (`crate::keyring`). Values are
//! never returned.
use super::*;

#[derive(Default, Deserialize)]
#[serde(default)]
struct MoveBody {
    name: Text,
    /// `keyring` or `file`.
    to: Text,
}

impl Service {
    pub(super) async fn secrets_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/secrets") => {
                self.blocking(call, |service, _| {
                    crate::keyring::overview(service.engine.paths())
                })
                .await
            }
            ("POST", "/api/secrets/move") => {
                self.blocking(call, |service, call| {
                    let body: MoveBody = call.body()?;
                    let name = body.name.as_str();
                    crate::keyring::check_name(name)?;
                    let paths = service.engine.paths();
                    match body.to.as_str() {
                        "keyring" => crate::keyring::move_in(paths, name)?,
                        "file" => crate::keyring::move_out(paths, name)?,
                        _ => bail!("Choose keyring or file"),
                    }
                    crate::keyring::overview(paths)
                })
                .await
            }
            _ => Err(call.unavailable()),
        }
    }
}
