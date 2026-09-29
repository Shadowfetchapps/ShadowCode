// Compare (0.31.0 and later): a finished comparison of two lanes, stored the
// way `compare::save` stores it. Lanes reuse the fixture conversations.
async fn compare(ctx: &mut Ctx) {
    use shadowcode_core::compare::Record;
    let id = "c0ffee00c0ffee00c0ffee00c0ffee00";
    let second = ctx.manifest.get("second_session_id").and_then(Value::as_str).unwrap_or("").to_owned();
    let jobs = ctx.job_ids.clone();
    let record: Result<Record, _> = serde_json::from_value(json!({
        "id": id,
        "workspace": ctx.project,
        "task": "Write the fixture notes",
        "mode": "code",
        "created_at": 1_759_000_100.0,
        "finished_at": 1_759_000_200.0,
        "state": "done",
        "lanes": [
            {"model": "local:gguf:fixture-a", "name": "Fixture A", "session_id": ctx.session_id,
             "job_id": jobs.first(), "status": "completed", "summary": "Wrote notes.txt",
             "duration_s": 12.5, "usage": {"total_tokens": 300}, "removed": true},
            {"model": "local:gguf:fixture-b", "name": "Fixture B", "session_id": second,
             "job_id": jobs.last(), "status": "completed", "summary": "Wrote notes.txt too",
             "duration_s": 14.0, "usage": {"total_tokens": 320}, "removed": true}
        ],
    }));
    let record = match record {
        Ok(record) => record,
        Err(error) => return ctx.skip("compare record", error),
    };
    let store = ctx.service.engine.store();
    let text = serde_json::to_string(&record).unwrap();
    let saved = store
        .set_native_meta(&format!("compare:{id}"), &text)
        .and_then(|_| {
            store.set_native_meta(
                &format!("compare_index:{}", ctx.project.display()),
                &json!([id]).to_string(),
            )
        })
        .and_then(|_| {
            store.set_native_meta(
                &format!("compare_scoreboard:{}", ctx.project.display()),
                &json!([
                    {"model": "local:gguf:fixture-a", "name": "Fixture A", "wins": 1, "runs": 1},
                    {"model": "local:gguf:fixture-b", "name": "Fixture B", "wins": 0, "runs": 1}
                ])
                .to_string(),
            )
        })
        .and_then(|_| store.set_session_meta(&ctx.session_id, "compare_id", id))
        .and_then(|_| store.set_session_meta(&ctx.session_id, "compare_lane", "local:gguf:fixture-a"));
    match saved {
        Ok(()) => ctx.record("compare_id", json!(id)),
        Err(error) => ctx.skip("compare record", error),
    }
}
