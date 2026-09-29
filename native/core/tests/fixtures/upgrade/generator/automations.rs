// Automations (0.32.0 and later): one scheduled automation that ran once in
// the project folder, and one paused automation.
async fn automations(ctx: &mut Ctx) {
    let draft = json!({
        "workspace": ctx.project, "name": "Morning notes", "prompt": "Refresh the fixture notes",
        "model": "", "mode": "code",
        "schedule": {"kind": "daily", "time": "09:00"}, "timezone": "local",
        "options": {"checkout": "main", "permission": "project", "on_approval": "stop",
                    "max_runtime_minutes": 10, "catch_up_minutes": 0, "notify": false},
    });
    let automation = match ctx.call("POST", "/api/automations", draft).await {
        Ok(automation) => automation,
        Err(error) => return ctx.skip("automation", error),
    };
    let id = automation["id"].as_str().unwrap_or_default().to_owned();
    ctx.record("automation_id", json!(id));
    match ctx.call("POST", &format!("/api/automations/{id}/run"), json!({})).await {
        Ok(run) => {
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                let view = ctx
                    .call("GET", &format!("/api/automations/{id}"), Value::Null)
                    .await
                    .unwrap_or(Value::Null);
                if view["running_run"].is_null() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            ctx.record("automation_run_id", run["id"].clone());
        }
        Err(error) => ctx.skip("automation run", error),
    }
    let paused = json!({
        "workspace": ctx.project, "name": "Weekly check", "prompt": "Check the project",
        "model": "", "mode": "plan",
        "schedule": {"kind": "weekly", "day": 1, "time": "08:30"}, "timezone": "utc",
        "options": {"checkout": "main"},
    });
    match ctx.call("POST", "/api/automations", paused).await {
        Ok(created) => {
            let id = created["id"].as_str().unwrap_or_default().to_owned();
            let _ = ctx.call("POST", &format!("/api/automations/{id}/pause"), json!({})).await;
            ctx.record("paused_automation_id", json!(id));
        }
        Err(error) => ctx.skip("paused automation", error),
    }
}
