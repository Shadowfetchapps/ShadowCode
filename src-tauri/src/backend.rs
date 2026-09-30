use anyhow::{bail, Result};
use serde_json::Value;
use shadowcode_core::{
    control,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{path::PathBuf, time::Duration};

pub enum Backend {
    Owned {
        service: Service,
        server: control::Server,
    },
    Attached(control::ViewClient),
}
impl Backend {
    pub async fn open(paths: AppPaths, workspace: Option<PathBuf>) -> Result<Self> {
        let workspace = workspace
            .or_else(|| paths.remembered_workspace())
            .unwrap_or(std::env::current_dir()?);
        let client = control::Endpoint::for_paths(&paths)?.client(workspace.clone(), None);
        for attempt in 0..30 {
            if client.available().await? {
                return Ok(Self::Attached(client.open_view().await?));
            }
            match Service::open(paths.clone(), Some(workspace.clone())) {
                Ok(service) => {
                    let server = control::Server::start(service.clone())?;
                    // The engine owner runs scheduled automations.
                    service.engine.start_automations();
                    return Ok(Self::Owned { service, server });
                }
                Err(error) if attempt < 29 && error.to_string().contains("already running") => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        }
        bail!("The active engine has not opened its local connection; wait for startup or close the other version")
    }
    pub async fn dispatch(&self, request: Request) -> Result<Value> {
        // Settings › Your data also needs to know: quitting an attached
        // window does not let a scheduled restore or reset run.
        let describe = request.method == "GET"
            && matches!(
                request.path.as_str(),
                "/api/health" | "/api/version" | "/api/data"
            );
        let mut result = match self {
            Self::Owned { service, .. } => service.dispatch(request).await?,
            Self::Attached(view) => self.dispatch_attached(view, request).await?,
        };
        if describe {
            result["desktop_attached"] = Value::Bool(matches!(self, Self::Attached(_)));
            result["desktop_pid"] = serde_json::json!(std::process::id());
        }
        Ok(result)
    }
    async fn dispatch_attached(
        &self,
        view: &control::ViewClient,
        request: Request,
    ) -> Result<Value> {
        if view.disconnected() {
            view.reattach().await?;
            // The request never reached the previous engine, including Trust
            // and open. Retry it on the new view.
            return view.dispatch(request).await;
        }
        match view.dispatch(request.clone()).await {
            Ok(result) => Ok(result),
            Err(error) if engine_gone(&error) => {
                view.mark_disconnected();
                view.reattach().await?;
                if retry_after_uncertain_reattach(&request) {
                    view.dispatch(request).await
                } else {
                    bail!(
                        "Engine reattached after a process restart. The previous request was not retried."
                    )
                }
            }
            Err(error) => Err(error),
        }
    }
    /// Reconnect a disconnected attached view. Never starts jobs or replays tools.
    pub async fn reattach_if_needed(&self) -> Result<bool> {
        match self {
            Self::Owned { .. } => Ok(false),
            Self::Attached(view) if view.disconnected() => {
                view.reattach().await?;
                Ok(true)
            }
            Self::Attached(_) => Ok(false),
        }
    }
    pub async fn close(&self) -> Result<()> {
        match self {
            Self::Owned { service, server } => {
                server.close();
                server.wait_closed().await;
                // The user's own terminals end with the window.
                let terminals = service.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    terminals.close_terminals(Duration::from_millis(800))
                })
                .await;
                service.engine.shutdown().await
            }
            Self::Attached(view) => {
                // The shared owner may already have exited. Detaching drops
                // the lease even when its acknowledgement cannot arrive; that
                // must not trap the user in an uncloseable window.
                if let Err(error) = view.close().await {
                    eprintln!("Detached desktop without engine acknowledgement: {error:#}");
                }
                Ok(())
            }
        }
    }
}

fn retry_after_uncertain_reattach(request: &Request) -> bool {
    request.method == "GET"
        || matches!(
            request.path.as_str(),
            "/api/projects" | "/api/projects/trust" | "/api/onboarding"
        )
}

fn engine_gone(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text.contains("Attached view is closed")
        || text.contains("No running engine")
        || error.downcast_ref::<std::io::Error>().is_some_and(|error| {
            matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn req(method: &str, path: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            body: json!({}),
        }
    }
    #[test]
    fn trust_and_open_retries_after_reattach() {
        assert!(retry_after_uncertain_reattach(&req(
            "POST",
            "/api/projects/trust"
        )));
        assert!(retry_after_uncertain_reattach(&req(
            "POST",
            "/api/projects"
        )));
        assert!(retry_after_uncertain_reattach(&req("GET", "/api/health")));
        assert!(!retry_after_uncertain_reattach(&req("POST", "/api/jobs")));
    }
}
